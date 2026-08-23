use crate::{
    binary_cache::CACHE_INFO,
    round_manager::RoundManager,
    store::{EvidenceStore, MetricsSnapshot, RoundConfig},
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const PREFIX: &str = "reproductive_nix_cache";

#[derive(Clone)]
pub struct HttpMetrics {
    inner: Arc<HttpMetricsInner>,
}

#[must_use = "the guard must be held until request processing finishes"]
pub(crate) struct ActiveRequestGuard {
    inner: Arc<HttpMetricsInner>,
}

struct HttpMetricsInner {
    started_at: SystemTime,
    active: AtomicU64,
    observations: Mutex<BTreeMap<HttpLabels, HttpObservation>>,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct HttpLabels {
    method: String,
    route: String,
    status: u16,
}

#[derive(Clone, Copy, Default)]
struct HttpObservation {
    requests: u64,
    duration: Duration,
}

#[derive(Clone)]
pub struct ServerMetadata {
    pub listen: String,
    pub database: String,
}

impl Default for ServerMetadata {
    fn default() -> Self {
        Self {
            listen: "unknown".into(),
            database: "in-memory".into(),
        }
    }
}

impl Default for HttpMetrics {
    fn default() -> Self {
        Self {
            inner: Arc::new(HttpMetricsInner {
                started_at: SystemTime::now(),
                active: AtomicU64::new(0),
                observations: Mutex::new(BTreeMap::new()),
            }),
        }
    }
}

impl HttpMetrics {
    pub(crate) fn begin_request(&self) -> ActiveRequestGuard {
        self.inner.active.fetch_add(1, Ordering::Relaxed);
        ActiveRequestGuard {
            inner: Arc::clone(&self.inner),
        }
    }

    pub(crate) fn observe_request(
        &self,
        method: &str,
        route: &str,
        status: u16,
        duration: Duration,
    ) {
        let mut observations = lock(&self.inner.observations);
        let observation = observations
            .entry(HttpLabels {
                method: method.into(),
                route: route.into(),
                status,
            })
            .or_default();
        observation.requests += 1;
        observation.duration += duration;
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct ExportContext<'a> {
    pub store: &'a EvidenceStore,
    pub round_config: RoundConfig,
    pub cache_minimum_builders: Option<usize>,
    pub round_manager: Option<&'a RoundManager>,
    pub http: &'a HttpMetrics,
    pub server: &'a ServerMetadata,
}

pub async fn encode(context: ExportContext<'_>) -> Result<String> {
    let store = context.store.metrics_snapshot(context.round_config).await?;
    let mut output = String::new();

    descriptor(
        &mut output,
        "build_info",
        "Build information for the server.",
        "gauge",
    );
    sample(
        &mut output,
        "build_info",
        &[&("version", env!("CARGO_PKG_VERSION"))],
        1,
    );
    descriptor(
        &mut output,
        "process_start_time_seconds",
        "Unix time when the server process started.",
        "gauge",
    );
    sample(
        &mut output,
        "process_start_time_seconds",
        &[],
        context
            .http
            .inner
            .started_at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64(),
    );
    descriptor(
        &mut output,
        "server_config_info",
        "Static server configuration represented as labels.",
        "gauge",
    );
    sample(
        &mut output,
        "server_config_info",
        &[
            &("listen", context.server.listen.as_str()),
            &("database", context.server.database.as_str()),
        ],
        1,
    );
    gauge(
        &mut output,
        "commit_minimum_builders",
        "Distinct commitments required to start revealing.",
        context.round_config.minimum_builders,
    );
    gauge(
        &mut output,
        "commit_window_seconds",
        "Configured commit window in seconds.",
        context.round_config.commit_window.num_seconds(),
    );
    gauge(
        &mut output,
        "reveal_window_seconds",
        "Configured reveal window in seconds.",
        context.round_config.reveal_window.num_seconds(),
    );
    gauge(
        &mut output,
        "binary_cache_enabled",
        "Whether the consensus binary cache gateway is enabled.",
        usize::from(context.cache_minimum_builders.is_some()),
    );
    if let Some(minimum_builders) = context.cache_minimum_builders {
        gauge(
            &mut output,
            "cache_minimum_builders",
            "Distinct matching builders required to publish a cache output.",
            minimum_builders,
        );
        descriptor(
            &mut output,
            "binary_cache_info",
            "Nix binary cache configuration represented as labels.",
            "gauge",
        );
        sample(
            &mut output,
            "binary_cache_info",
            &[&("contents", CACHE_INFO)],
            1,
        );
    }

    encode_http(&mut output, context.http);
    encode_round_manager(&mut output, context.round_manager);
    encode_store(&mut output, &store);
    Ok(output)
}

fn encode_http(output: &mut String, metrics: &HttpMetrics) {
    gauge(
        output,
        "http_requests_active",
        "HTTP requests currently being served.",
        metrics.inner.active.load(Ordering::Relaxed),
    );
    let observations = lock(&metrics.inner.observations);
    descriptor(
        output,
        "http_requests_total",
        "HTTP requests completed by method, matched route, and status.",
        "counter",
    );
    descriptor(
        output,
        "http_request_duration_seconds",
        "HTTP request duration by method, matched route, and status.",
        "summary",
    );
    for (labels, observation) in observations.iter() {
        let status = labels.status.to_string();
        let labels = [
            ("method", labels.method.as_str()),
            ("route", labels.route.as_str()),
            ("status", status.as_str()),
        ];
        sample(
            output,
            "http_requests_total",
            &labels.iter().collect::<Vec<_>>(),
            observation.requests,
        );
        sample(
            output,
            "http_request_duration_seconds_sum",
            &labels.iter().collect::<Vec<_>>(),
            observation.duration.as_secs_f64(),
        );
        sample(
            output,
            "http_request_duration_seconds_count",
            &labels.iter().collect::<Vec<_>>(),
            observation.requests,
        );
    }
}

fn encode_round_manager(output: &mut String, manager: Option<&RoundManager>) {
    gauge(
        output,
        "round_manager_enabled",
        "Whether builder-node dispatch is configured.",
        usize::from(manager.is_some()),
    );
    let Some(manager) = manager else {
        return;
    };
    let snapshot = manager.metrics_snapshot();
    gauge(
        output,
        "build_queue_capacity",
        "Maximum number of build commands waiting in memory.",
        snapshot.queue_capacity,
    );
    gauge(
        output,
        "build_queue_length",
        "Build commands currently waiting in memory.",
        snapshot.queued.len(),
    );
    gauge(
        output,
        "build_jobs_active",
        "Build dispatch jobs currently active.",
        usize::from(snapshot.active.is_some()),
    );
    counter(
        output,
        "build_jobs_accepted_total",
        "Build jobs accepted into the in-memory queue.",
        snapshot.jobs_accepted,
    );
    descriptor(
        output,
        "build_jobs_rejected_total",
        "Build jobs rejected before queueing.",
        "counter",
    );
    sample(
        output,
        "build_jobs_rejected_total",
        &[&("reason", "full")],
        snapshot.jobs_rejected_full,
    );
    sample(
        output,
        "build_jobs_rejected_total",
        &[&("reason", "closed")],
        snapshot.jobs_rejected_closed,
    );
    counter(
        output,
        "build_jobs_started_total",
        "Build jobs removed from the queue and started.",
        snapshot.jobs_started,
    );
    counter(
        output,
        "build_jobs_completed_total",
        "Build jobs for which all builder-node dispatches finished.",
        snapshot.jobs_completed,
    );
    descriptor(
        output,
        "build_job_info",
        "Queued and active build commands represented as labels.",
        "gauge",
    );
    for job in snapshot.queued.iter().chain(snapshot.active.iter()) {
        let job_id = job.job_id.to_string();
        let state = if snapshot
            .active
            .as_ref()
            .is_some_and(|active| active.job_id == job.job_id)
        {
            "active"
        } else {
            "queued"
        };
        let substitute = job.command.substitute.to_string();
        let claims = serde_json::to_string(&job.command.claims).unwrap_or_default();
        sample(
            output,
            "build_job_info",
            &[
                &("job_id", job_id.as_str()),
                &("state", state),
                &("package_ref", job.command.package_ref.as_str()),
                &("substitute", substitute.as_str()),
                &("claims", claims.as_str()),
            ],
            1,
        );
    }
    descriptor(
        output,
        "builder_node_info",
        "Configured builder nodes represented as labels.",
        "gauge",
    );
    descriptor(
        output,
        "builder_node_dispatches_total",
        "Builder-node dispatch attempts.",
        "counter",
    );
    descriptor(
        output,
        "builder_node_requests_active",
        "Build requests currently being served by each builder node.",
        "gauge",
    );
    descriptor(
        output,
        "builder_node_results_total",
        "Builder-node dispatch results.",
        "counter",
    );
    for node in snapshot.nodes {
        sample(
            output,
            "builder_node_info",
            &[&("node", node.node.as_str())],
            1,
        );
        sample(
            output,
            "builder_node_dispatches_total",
            &[&("node", node.node.as_str())],
            node.dispatched,
        );
        sample(
            output,
            "builder_node_requests_active",
            &[&("node", node.node.as_str())],
            node.active,
        );
        sample(
            output,
            "builder_node_results_total",
            &[&("node", node.node.as_str()), &("result", "success")],
            node.succeeded,
        );
        sample(
            output,
            "builder_node_results_total",
            &[&("node", node.node.as_str()), &("result", "failure")],
            node.failed,
        );
    }
}

fn encode_store(output: &mut String, snapshot: &MetricsSnapshot) {
    descriptor(
        output,
        "store_records",
        "Rows currently stored in each normalized SQLite table.",
        "gauge",
    );
    for (table, count) in [
        ("commit_reveal_rounds", snapshot.rounds.len()),
        ("evidence_commitments", snapshot.commitments.len()),
        ("evidences", snapshot.evidences.len()),
        ("cache_locations", snapshot.cache_locations.len()),
        ("claims", snapshot.claims.len()),
        ("build_claims", snapshot.build_claims.len()),
        ("build_outputs", snapshot.build_outputs.len()),
        ("log_claims", snapshot.log_claims.len()),
    ] {
        sample(output, "store_records", &[&("table", table)], count);
    }

    descriptor(
        output,
        "round_info",
        "Stored commit-reveal rounds.",
        "gauge",
    );
    descriptor(
        output,
        "round_started_time_seconds",
        "Round start time.",
        "gauge",
    );
    descriptor(
        output,
        "round_commit_deadline_seconds",
        "Round commit deadline.",
        "gauge",
    );
    descriptor(
        output,
        "round_reveal_started_time_seconds",
        "Round reveal start time.",
        "gauge",
    );
    descriptor(
        output,
        "round_reveal_deadline_seconds",
        "Round reveal deadline.",
        "gauge",
    );
    descriptor(
        output,
        "round_closed_time_seconds",
        "Round close time.",
        "gauge",
    );
    descriptor(
        output,
        "round_commitments",
        "Commitments currently stored for a round.",
        "gauge",
    );
    descriptor(
        output,
        "round_reveals",
        "Revealed evidence currently stored for a round.",
        "gauge",
    );
    for round in &snapshot.rounds {
        let id = round.id.to_string();
        let phase = if round.closed_at.is_some() {
            if round.expired {
                "expired"
            } else {
                "completed"
            }
        } else if round.reveal_started_at.is_some() {
            "revealing"
        } else {
            "committing"
        };
        let expired = round.expired.to_string();
        let labels = [
            ("round_id", id.as_str()),
            ("derivation_path", round.derivation_path.as_str()),
            ("phase", phase),
            ("expired", expired.as_str()),
        ];
        sample(output, "round_info", &labels.iter().collect::<Vec<_>>(), 1);
        id_time_sample(output, "round_started_time_seconds", &id, round.started_at);
        id_time_sample(
            output,
            "round_commit_deadline_seconds",
            &id,
            round.commit_deadline,
        );
        optional_id_time_sample(
            output,
            "round_reveal_started_time_seconds",
            &id,
            round.reveal_started_at,
        );
        optional_id_time_sample(
            output,
            "round_reveal_deadline_seconds",
            &id,
            round.reveal_deadline,
        );
        optional_id_time_sample(output, "round_closed_time_seconds", &id, round.closed_at);
        let commitments = snapshot
            .commitments
            .iter()
            .filter(|commitment| commitment.round_id == round.id)
            .count();
        let reveals = snapshot
            .commitments
            .iter()
            .filter(|commitment| {
                commitment.round_id == round.id && commitment.evidence_id.is_some()
            })
            .count();
        sample(
            output,
            "round_commitments",
            &[&("round_id", id.as_str())],
            commitments,
        );
        sample(
            output,
            "round_reveals",
            &[&("round_id", id.as_str())],
            reveals,
        );
    }

    descriptor(
        output,
        "commitment_info",
        "Stored evidence commitments.",
        "gauge",
    );
    descriptor(
        output,
        "commitment_committed_time_seconds",
        "Commitment creation time.",
        "gauge",
    );
    descriptor(
        output,
        "commitment_revealed_time_seconds",
        "Commitment reveal time.",
        "gauge",
    );
    for commitment in &snapshot.commitments {
        let id = commitment.id.to_string();
        let round_id = commitment.round_id.to_string();
        let evidence_id = optional_i64(commitment.evidence_id);
        sample(
            output,
            "commitment_info",
            &[
                &("commitment_id", id.as_str()),
                &("round_id", round_id.as_str()),
                &("builder_id", commitment.builder_id.as_str()),
                &("digest", commitment.digest.as_str()),
                &("nonce", commitment.nonce.as_deref().unwrap_or("")),
                &("evidence_id", evidence_id.as_str()),
            ],
            1,
        );
        named_id_time_sample(
            output,
            "commitment_committed_time_seconds",
            "commitment_id",
            &id,
            commitment.committed_at,
        );
        optional_named_id_time_sample(
            output,
            "commitment_revealed_time_seconds",
            "commitment_id",
            &id,
            commitment.revealed_at,
        );
    }

    descriptor(
        output,
        "evidence_info",
        "Stored evidence envelopes.",
        "gauge",
    );
    descriptor(
        output,
        "evidence_received_time_seconds",
        "Evidence receive time.",
        "gauge",
    );
    for evidence in &snapshot.evidences {
        let id = evidence.id.to_string();
        let schema_version = evidence.schema_version.to_string();
        sample(
            output,
            "evidence_info",
            &[
                &("evidence_id", id.as_str()),
                &("schema_version", schema_version.as_str()),
                &("builder_id", evidence.builder_id.as_str()),
                &("package_repository", evidence.package_repository.as_str()),
                &("package_name", evidence.package_name.as_str()),
            ],
            1,
        );
        named_id_time_sample(
            output,
            "evidence_received_time_seconds",
            "evidence_id",
            &id,
            evidence.received_at,
        );
    }

    descriptor(
        output,
        "cache_location_info",
        "Stored cache locations.",
        "gauge",
    );
    for location in &snapshot.cache_locations {
        let id = location.id.to_string();
        let evidence_id = location.evidence_id.to_string();
        let position = location.position.to_string();
        sample(
            output,
            "cache_location_info",
            &[
                &("location_id", id.as_str()),
                &("evidence_id", evidence_id.as_str()),
                &("position", position.as_str()),
                &("uri", location.uri.as_str()),
            ],
            1,
        );
    }

    descriptor(output, "claim_info", "Stored claim envelopes.", "gauge");
    for claim in &snapshot.claims {
        let id = claim.id.to_string();
        let evidence_id = claim.evidence_id.to_string();
        let position = claim.position.to_string();
        sample(
            output,
            "claim_info",
            &[
                &("claim_id", id.as_str()),
                &("evidence_id", evidence_id.as_str()),
                &("position", position.as_str()),
                &("kind", claim.kind.as_str()),
            ],
            1,
        );
    }

    descriptor(
        output,
        "build_claim_info",
        "Stored build claim payloads.",
        "gauge",
    );
    descriptor(
        output,
        "build_claim_built_time_seconds",
        "Build completion time from evidence.",
        "gauge",
    );
    for claim in &snapshot.build_claims {
        let id = claim.claim_id.to_string();
        sample(
            output,
            "build_claim_info",
            &[
                &("claim_id", id.as_str()),
                &("source_resolved_url", claim.source_resolved_url.as_str()),
                &(
                    "source_revision",
                    claim.source_revision.as_deref().unwrap_or(""),
                ),
                &(
                    "source_nar_hash",
                    claim.source_nar_hash.as_deref().unwrap_or(""),
                ),
                &("derivation_path", claim.derivation_path.as_str()),
                &(
                    "build_log_digest",
                    claim.build_log_digest.as_deref().unwrap_or(""),
                ),
                &("sbom_digest", claim.sbom_digest.as_deref().unwrap_or("")),
                &(
                    "test_result_digest",
                    claim.test_result_digest.as_deref().unwrap_or(""),
                ),
            ],
            1,
        );
        named_id_time_sample(
            output,
            "build_claim_built_time_seconds",
            "claim_id",
            &id,
            claim.built_at,
        );
    }

    descriptor(
        output,
        "build_output_info",
        "Stored build outputs.",
        "gauge",
    );
    descriptor(
        output,
        "build_output_nar_size_bytes",
        "Uncompressed NAR size from evidence.",
        "gauge",
    );
    for build_output in &snapshot.build_outputs {
        let claim_id = build_output.claim_id.to_string();
        let position = build_output.position.to_string();
        let labels = [
            ("claim_id", claim_id.as_str()),
            ("position", position.as_str()),
            ("output_name", build_output.output_name.as_str()),
            ("output_store_path", build_output.output_store_path.as_str()),
            ("nar_hash", build_output.nar_hash.as_str()),
            ("references_json", build_output.references_json.as_str()),
            ("closure_root", build_output.closure_root.as_str()),
            (
                "content_addressed",
                build_output.content_addressed.as_deref().unwrap_or(""),
            ),
        ];
        sample(
            output,
            "build_output_info",
            &labels.iter().collect::<Vec<_>>(),
            1,
        );
        sample(
            output,
            "build_output_nar_size_bytes",
            &[
                &("claim_id", claim_id.as_str()),
                &("position", position.as_str()),
            ],
            build_output.nar_size,
        );
    }

    descriptor(
        output,
        "log_claim_info",
        "Stored build stdout and stderr.",
        "gauge",
    );
    descriptor(
        output,
        "log_claim_started_time_seconds",
        "Log claim start time.",
        "gauge",
    );
    descriptor(
        output,
        "log_claim_finished_time_seconds",
        "Log claim finish time.",
        "gauge",
    );
    descriptor(
        output,
        "log_claim_stream_size_bytes",
        "Stored log stream size in bytes.",
        "gauge",
    );
    for claim in &snapshot.log_claims {
        let id = claim.claim_id.to_string();
        sample(
            output,
            "log_claim_info",
            &[
                &("claim_id", id.as_str()),
                &("stdout", claim.stdout.as_str()),
                &("stderr", claim.stderr.as_str()),
            ],
            1,
        );
        named_id_time_sample(
            output,
            "log_claim_started_time_seconds",
            "claim_id",
            &id,
            claim.started_at,
        );
        named_id_time_sample(
            output,
            "log_claim_finished_time_seconds",
            "claim_id",
            &id,
            claim.finished_at,
        );
        sample(
            output,
            "log_claim_stream_size_bytes",
            &[&("claim_id", id.as_str()), &("stream", "stdout")],
            claim.stdout.len(),
        );
        sample(
            output,
            "log_claim_stream_size_bytes",
            &[&("claim_id", id.as_str()), &("stream", "stderr")],
            claim.stderr.len(),
        );
    }
}

fn id_time_sample(output: &mut String, name: &str, id: &str, value: DateTime<Utc>) {
    named_id_time_sample(output, name, "round_id", id, value);
}

fn optional_id_time_sample(
    output: &mut String,
    name: &str,
    id: &str,
    value: Option<DateTime<Utc>>,
) {
    optional_named_id_time_sample(output, name, "round_id", id, value);
}

fn named_id_time_sample(
    output: &mut String,
    name: &str,
    id_name: &str,
    id: &str,
    value: DateTime<Utc>,
) {
    sample(output, name, &[&(id_name, id)], timestamp(value));
}

fn optional_named_id_time_sample(
    output: &mut String,
    name: &str,
    id_name: &str,
    id: &str,
    value: Option<DateTime<Utc>>,
) {
    if let Some(value) = value {
        named_id_time_sample(output, name, id_name, id, value);
    }
}

fn timestamp(value: DateTime<Utc>) -> f64 {
    value.timestamp_micros() as f64 / 1_000_000.0
}

fn optional_i64(value: Option<i64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn gauge(output: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    descriptor(output, name, help, "gauge");
    sample(output, name, &[], value);
}

fn counter(output: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    descriptor(output, name, help, "counter");
    sample(output, name, &[], value);
}

fn descriptor(output: &mut String, name: &str, help: &str, metric_type: &str) {
    let name = full_name(name);
    writeln!(output, "# HELP {name} {}", escape_help(help)).unwrap();
    writeln!(output, "# TYPE {name} {metric_type}").unwrap();
}

fn sample(
    output: &mut String,
    name: &str,
    labels: &[&(&str, &str)],
    value: impl std::fmt::Display,
) {
    output.push_str(&full_name(name));
    if !labels.is_empty() {
        output.push('{');
        for (index, (name, value)) in labels.iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            write!(output, "{name}=\"{}\"", escape_label(value)).unwrap();
        }
        output.push('}');
    }
    writeln!(output, " {value}").unwrap();
}

fn full_name(name: &str) -> String {
    format!("{PREFIX}_{name}")
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn escape_help(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_prometheus_label_values() {
        assert_eq!(escape_label("a\\b\n\"c\""), "a\\\\b\\n\\\"c\\\"");
    }

    #[test]
    fn active_request_guard_decrements_when_dropped() {
        let metrics = HttpMetrics::default();
        assert_eq!(metrics.inner.active.load(Ordering::Relaxed), 0);

        let guard = metrics.begin_request();
        assert_eq!(metrics.inner.active.load(Ordering::Relaxed), 1);

        metrics.observe_request("GET", "/", 200, Duration::from_millis(1));
        assert_eq!(metrics.inner.active.load(Ordering::Relaxed), 1);

        drop(guard);
        assert_eq!(metrics.inner.active.load(Ordering::Relaxed), 0);
    }
}
