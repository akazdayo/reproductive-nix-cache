use anyhow::{Result, bail};
use reqwest::{Client, Url};
use shared::{BuildCommand, BuildDispatchOutcome, BuildDispatchResponse, BuildNodeReceipt};
use std::{collections::BTreeSet, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct RoundManager {
    nodes: Arc<[Url]>,
    token: Arc<str>,
    client: Client,
}

impl RoundManager {
    pub fn new(nodes: Vec<Url>, token: String) -> Result<Self> {
        if nodes.is_empty() {
            bail!("round manager requires at least one builder node");
        }
        if token.is_empty() {
            bail!("builder node token must not be empty");
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
            .build()?;
        Ok(Self {
            nodes: nodes.into(),
            token: token.into(),
            client,
        })
    }

    pub async fn dispatch(&self, command: &BuildCommand) -> BuildDispatchResponse {
        let mut tasks = Vec::with_capacity(self.nodes.len());
        for node in self.nodes.iter().cloned() {
            let client = self.client.clone();
            let token = Arc::clone(&self.token);
            let command = command.clone();
            tasks.push(tokio::spawn(async move {
                dispatch_one(client, node, token, command).await
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

async fn dispatch_one(
    client: Client,
    node: Url,
    token: Arc<str>,
    command: BuildCommand,
) -> BuildDispatchOutcome {
    let node_name = node.to_string();
    let endpoint = match node.join("v1/builds") {
        Ok(endpoint) => endpoint,
        Err(error) => return BuildDispatchOutcome::failed(node_name, error.to_string()),
    };
    let response = match client
        .post(endpoint)
        .bearer_auth(token.as_ref())
        .json(&command)
        .send()
        .await
    {
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
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    async fn spawn_node(
        status: StatusCode,
        receipt: BuildNodeReceipt,
        received: Arc<Mutex<Vec<BuildCommand>>>,
    ) -> Url {
        let app = Router::new().route(
            "/v1/builds",
            post(
                move |headers: HeaderMap, Json(command): Json<BuildCommand>| {
                    let received = Arc::clone(&received);
                    let receipt = receipt.clone();
                    async move {
                        assert_eq!(
                            headers.get(header::AUTHORIZATION).unwrap(),
                            "Bearer builder-secret"
                        );
                        received.lock().unwrap().push(command);
                        (status, Json(receipt)).into_response()
                    }
                },
            ),
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
        let manager = RoundManager::new(vec![first, second], "builder-secret".into()).unwrap();
        let command = BuildCommand {
            package_ref: "nixpkgs#hello".into(),
            substitute: false,
            claims: vec![],
        };

        let result = manager.dispatch(&command).await;

        assert_eq!(result.builders.len(), 2);
        assert!(result.builders[0].success);
        assert!(!result.builders[1].success);
        assert_eq!(*received.lock().unwrap(), vec![command.clone(), command]);
    }

    #[test]
    fn rejects_unsafe_builder_node_urls() {
        let url = Url::parse("https://user:secret@example.com/path").unwrap();
        assert!(RoundManager::new(vec![url], "token".into()).is_err());

        let url = Url::parse("https://builder.example.com/").unwrap();
        assert!(RoundManager::new(vec![url.clone(), url], "token".into()).is_err());
    }
}
