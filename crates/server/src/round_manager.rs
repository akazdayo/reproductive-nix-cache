use anyhow::{Result, bail};
use reqwest::{Client, Url};
use shared::{
    BuildCommand, BuildDispatchOutcome, BuildDispatchResponse, BuildNodeReceipt, BuildQueueReceipt,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
use tracing::{error, info};

const BUILDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct RoundManager {
    sender: mpsc::Sender<QueuedBuild>,
    next_job_id: Arc<AtomicU64>,
    metrics: Arc<Mutex<RuntimeMetrics>>,
}

struct Dispatcher {
    nodes: Arc<[Url]>,
    client: Client,
}

struct QueuedBuild {
    job_id: u64,
    command: BuildCommand,
}

#[derive(Default)]
struct RuntimeMetrics {
    queued: BTreeMap<u64, BuildCommand>,
    active: Option<(u64, BuildCommand)>,
    jobs_accepted: u64,
    jobs_rejected_full: u64,
    jobs_rejected_closed: u64,
    jobs_started: u64,
    jobs_completed: u64,
    nodes: BTreeMap<String, NodeMetrics>,
}

#[derive(Clone, Default)]
struct NodeMetrics {
    active: u64,
    dispatched: u64,
    succeeded: u64,
    failed: u64,
}

#[derive(Clone)]
pub struct BuildJobSnapshot {
    pub job_id: u64,
    pub command: BuildCommand,
}

#[derive(Clone)]
pub struct BuilderNodeSnapshot {
    pub node: String,
    pub active: u64,
    pub dispatched: u64,
    pub succeeded: u64,
    pub failed: u64,
}

pub struct RoundManagerSnapshot {
    pub queue_capacity: usize,
    pub queued: Vec<BuildJobSnapshot>,
    pub active: Option<BuildJobSnapshot>,
    pub jobs_accepted: u64,
    pub jobs_rejected_full: u64,
    pub jobs_rejected_closed: u64,
    pub jobs_started: u64,
    pub jobs_completed: u64,
    pub nodes: Vec<BuilderNodeSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueError {
    Full,
    Closed,
}

impl fmt::Display for EnqueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Full => "build queue is full",
            Self::Closed => "build queue worker is unavailable",
        })
    }
}

impl std::error::Error for EnqueueError {}

impl RoundManager {
    pub fn new(nodes: Vec<Url>, queue_capacity: usize) -> Result<Self> {
        if queue_capacity == 0 {
            bail!("build queue capacity must be at least 1");
        }
        let dispatcher = Dispatcher::new(nodes)?;
        let metrics = Arc::new(Mutex::new(RuntimeMetrics {
            nodes: dispatcher
                .nodes
                .iter()
                .map(|node| (node.to_string(), NodeMetrics::default()))
                .collect(),
            ..RuntimeMetrics::default()
        }));
        let (sender, receiver) = mpsc::channel(queue_capacity);
        tokio::spawn(run_worker(receiver, dispatcher, Arc::clone(&metrics)));
        Ok(Self {
            sender,
            next_job_id: Arc::new(AtomicU64::new(1)),
            metrics,
        })
    }

    pub fn enqueue(&self, command: BuildCommand) -> Result<BuildQueueReceipt, EnqueueError> {
        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let mut metrics = lock_metrics(&self.metrics);
        metrics.queued.insert(job_id, command.clone());
        match self.sender.try_send(QueuedBuild { job_id, command }) {
            Ok(()) => {
                metrics.jobs_accepted += 1;
                Ok(BuildQueueReceipt {
                    job_id,
                    queued: true,
                })
            }
            Err(error) => {
                metrics.queued.remove(&job_id);
                let error = match error {
                    mpsc::error::TrySendError::Full(_) => {
                        metrics.jobs_rejected_full += 1;
                        EnqueueError::Full
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        metrics.jobs_rejected_closed += 1;
                        EnqueueError::Closed
                    }
                };
                Err(error)
            }
        }
    }

    pub fn metrics_snapshot(&self) -> RoundManagerSnapshot {
        let metrics = lock_metrics(&self.metrics);
        RoundManagerSnapshot {
            queue_capacity: self.sender.max_capacity(),
            queued: metrics
                .queued
                .iter()
                .map(|(&job_id, command)| BuildJobSnapshot {
                    job_id,
                    command: command.clone(),
                })
                .collect(),
            active: metrics
                .active
                .as_ref()
                .map(|(job_id, command)| BuildJobSnapshot {
                    job_id: *job_id,
                    command: command.clone(),
                }),
            jobs_accepted: metrics.jobs_accepted,
            jobs_rejected_full: metrics.jobs_rejected_full,
            jobs_rejected_closed: metrics.jobs_rejected_closed,
            jobs_started: metrics.jobs_started,
            jobs_completed: metrics.jobs_completed,
            nodes: metrics
                .nodes
                .iter()
                .map(|(node, values)| BuilderNodeSnapshot {
                    node: node.clone(),
                    active: values.active,
                    dispatched: values.dispatched,
                    succeeded: values.succeeded,
                    failed: values.failed,
                })
                .collect(),
        }
    }
}

fn lock_metrics(metrics: &Mutex<RuntimeMetrics>) -> MutexGuard<'_, RuntimeMetrics> {
    metrics
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Dispatcher {
    fn new(nodes: Vec<Url>) -> Result<Self> {
        if nodes.is_empty() {
            bail!("round manager requires at least one builder node");
        }
        let mut unique = BTreeSet::new();
        for node in &nodes {
            if !matches!(node.scheme(), "http" | "https")
                || !node.username().is_empty()
                || node.password().is_some()
                || node.query().is_some()
                || node.fragment().is_some()
                || node.path() != "/"
            {
                bail!(
                    "builder node URL must be an HTTP(S) origin without credentials, path, query, or fragment: {node}"
                );
            }
            if !unique.insert(node.as_str()) {
                bail!("builder node URL is duplicated: {node}");
            }
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(BUILDER_REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            nodes: nodes.into(),
            client,
        })
    }

    async fn dispatch(&self, command: &BuildCommand) -> BuildDispatchResponse {
        let mut tasks = Vec::with_capacity(self.nodes.len());
        for node in self.nodes.iter().cloned() {
            let client = self.client.clone();
            let command = command.clone();
            tasks.push(tokio::spawn(async move {
                dispatch_one(client, node, command).await
            }));
        }

        let mut builders = Vec::with_capacity(tasks.len());
        for task in tasks {
            match task.await {
                Ok(outcome) => builders.push(outcome),
                Err(error) => builders.push(BuildDispatchOutcome::failed(
                    "<internal>",
                    format!("builder dispatch task failed: {error}"),
                )),
            }
        }
        BuildDispatchResponse { builders }
    }
}

async fn run_worker(
    mut receiver: mpsc::Receiver<QueuedBuild>,
    dispatcher: Dispatcher,
    metrics: Arc<Mutex<RuntimeMetrics>>,
) {
    while let Some(job) = receiver.recv().await {
        {
            let mut metrics = lock_metrics(&metrics);
            metrics.queued.remove(&job.job_id);
            metrics.active = Some((job.job_id, job.command.clone()));
            metrics.jobs_started += 1;
            for node in dispatcher.nodes.iter() {
                let node = metrics.nodes.entry(node.to_string()).or_default();
                node.active += 1;
                node.dispatched += 1;
            }
        }
        info!(
            job_id = job.job_id,
            package_ref = %job.command.package_ref,
            substitute = job.command.substitute,
            builders = dispatcher.nodes.len(),
            "build dispatch started"
        );
        let response = dispatcher.dispatch(&job.command).await;
        let mut succeeded = 0;
        for outcome in &response.builders {
            if outcome.success {
                succeeded += 1;
                info!(
                    job_id = job.job_id,
                    node = %outcome.node,
                    builder_id = outcome.builder_id.as_deref().unwrap_or("<unknown>"),
                    round_id = outcome.round_id,
                    evidence_id = outcome.evidence_id,
                    "builder completed build"
                );
            } else {
                error!(
                    job_id = job.job_id,
                    node = %outcome.node,
                    error = outcome.error.as_deref().unwrap_or("unknown error"),
                    "builder failed build"
                );
            }
        }
        {
            let mut metrics = lock_metrics(&metrics);
            for outcome in &response.builders {
                let node = metrics.nodes.entry(outcome.node.clone()).or_default();
                node.active = node.active.saturating_sub(1);
                if outcome.success {
                    node.succeeded += 1;
                } else {
                    node.failed += 1;
                }
            }
            metrics.active = None;
            metrics.jobs_completed += 1;
        }
        info!(
            job_id = job.job_id,
            succeeded,
            failed = dispatcher.nodes.len() - succeeded,
            "build dispatch finished"
        );
    }
}

async fn dispatch_one(client: Client, node: Url, command: BuildCommand) -> BuildDispatchOutcome {
    let node_name = node.to_string();
    let endpoint = match node.join("v1/builds") {
        Ok(endpoint) => endpoint,
        Err(error) => return BuildDispatchOutcome::failed(node_name, error.to_string()),
    };
    let response = match client.post(endpoint).json(&command).send().await {
        Ok(response) => response,
        Err(error) => {
            return BuildDispatchOutcome::failed(
                node_name,
                format!("failed to contact builder node: {error}"),
            );
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        let detail = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(1024)
            .collect::<String>();
        return BuildDispatchOutcome::failed(
            node_name,
            if detail.trim().is_empty() {
                format!("builder node returned HTTP {status}")
            } else {
                format!("builder node returned HTTP {status}: {detail}")
            },
        );
    }
    match response.json::<BuildNodeReceipt>().await {
        Ok(receipt) => BuildDispatchOutcome::succeeded(node_name, receipt),
        Err(error) => BuildDispatchOutcome::failed(
            node_name,
            format!("builder node returned an invalid receipt: {error}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::{net::TcpListener, sync::Notify};

    async fn spawn_node(
        status: StatusCode,
        receipt: BuildNodeReceipt,
        received: Arc<Mutex<Vec<BuildCommand>>>,
    ) -> Url {
        let app = Router::new().route(
            "/v1/builds",
            post(move |Json(command): Json<BuildCommand>| {
                let received = Arc::clone(&received);
                let receipt = receipt.clone();
                async move {
                    received.lock().unwrap().push(command);
                    (status, Json(receipt)).into_response()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Url::parse(&format!("http://{address}/")).unwrap()
    }

    #[tokio::test]
    async fn forwards_the_same_command_in_parallel_and_aggregates_failures() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let first = spawn_node(
            StatusCode::OK,
            BuildNodeReceipt {
                builder_id: "builder-a".into(),
                round_id: 7,
                evidence_id: 41,
            },
            Arc::clone(&received),
        )
        .await;
        let second = spawn_node(
            StatusCode::CONFLICT,
            BuildNodeReceipt {
                builder_id: "builder-b".into(),
                round_id: 7,
                evidence_id: 42,
            },
            Arc::clone(&received),
        )
        .await;
        let dispatcher = Dispatcher::new(vec![first, second]).unwrap();
        let command = BuildCommand {
            package_ref: "nixpkgs#hello".into(),
            substitute: true,
            claims: vec![],
        };

        let result = dispatcher.dispatch(&command).await;

        assert_eq!(result.builders.len(), 2);
        assert!(result.builders[0].success);
        assert!(!result.builders[1].success);
        assert_eq!(*received.lock().unwrap(), vec![command.clone(), command]);
    }

    #[tokio::test]
    async fn enqueue_returns_a_job_id_and_rejects_when_the_queue_is_full() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let block_first = Arc::new(AtomicBool::new(true));
        let app = Router::new().route(
            "/v1/builds",
            post({
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                move || {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    let block = block_first.swap(false, Ordering::SeqCst);
                    async move {
                        if block {
                            started.notify_one();
                            release.notified().await;
                        }
                        Json(BuildNodeReceipt {
                            builder_id: "builder-a".into(),
                            round_id: 7,
                            evidence_id: 41,
                        })
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let node = Url::parse(&format!("http://{address}/")).unwrap();
        let manager = RoundManager::new(vec![node], 1).unwrap();
        let command = BuildCommand {
            package_ref: "nixpkgs#hello".into(),
            substitute: false,
            claims: vec![],
        };

        let first = manager.enqueue(command.clone()).unwrap();
        assert_eq!(first.job_id, 1);
        assert!(first.queued);
        started.notified().await;

        let second = manager.enqueue(command.clone()).unwrap();
        assert_eq!(second.job_id, 2);
        assert_eq!(manager.enqueue(command), Err(EnqueueError::Full));
        let snapshot = manager.metrics_snapshot();
        assert_eq!(snapshot.queue_capacity, 1);
        assert_eq!(snapshot.queued.len(), 1);
        assert_eq!(snapshot.queued[0].job_id, 2);
        assert_eq!(snapshot.active.as_ref().unwrap().job_id, 1);
        assert_eq!(snapshot.jobs_accepted, 2);
        assert_eq!(snapshot.jobs_rejected_full, 1);
        assert_eq!(snapshot.jobs_started, 1);
        assert_eq!(snapshot.nodes[0].active, 1);
        assert_eq!(snapshot.nodes[0].dispatched, 1);
        release.notify_one();
    }

    #[test]
    fn rejects_unsafe_builder_node_urls() {
        let url = Url::parse("https://user:secret@example.com/path").unwrap();
        assert!(Dispatcher::new(vec![url]).is_err());

        let url = Url::parse("https://builder.example.com/").unwrap();
        assert!(Dispatcher::new(vec![url.clone(), url]).is_err());
    }
}
