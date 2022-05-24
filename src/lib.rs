use std::{collections::HashMap, sync::Arc};

use futures::StreamExt;
use k8s_openapi::{api::core::v1::Secret, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use kube::api::PostParams;
use kube::{
    api::{Api, ListParams, Patch, PatchParams, Resource},
    runtime::controller::{Action, Controller},
    CustomResource,
};
use log::{debug, error, info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::{
    sync::RwLock,
    time::{self, Duration},
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to create ConfigMap: {0}")]
    ConfigMapCreationFailed(#[source] kube::Error),
    #[error("MissingObjectKey: {0}")]
    MissingObjectKey(&'static str),
}

#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(group = "bond.ectobit.com", version = "v1alpha1", kind = "Source")]
#[kube(shortname = "src", status = "Status", namespaced)]
pub struct SourceSpec {
    secrets: Vec<SourceItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct SourceItem {
    name: String,
    destinations: Vec<DestinationItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct DestinationItem {
    namespace: String,
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct Status {
    ready: String,
}

pub struct Client {
    inner: kube::Client,
}

impl Client {
    pub fn new(client: kube::Client) -> Self {
        Self { inner: client }
    }

    pub async fn run(&self) {
        let state = Arc::new(RwLock::new(HashMap::new()));

        {
            let client = self.inner.clone();
            let state = state.clone();

            tokio::spawn(async move {
                Controller::new(Api::<Source>::all(client.clone()), ListParams::default())
                    .shutdown_on_signal()
                    .run(
                        reconcile_source,
                        error_policy,
                        Arc::new(Data { client, state }),
                    )
                    .for_each(|res| async move {
                        match res {
                            Ok(_) => (),
                            Err(e) => warn!("reconcile failed: {}", e),
                        }
                    })
                    .await;
            });
        }

        time::sleep(Duration::from_secs(5)).await;

        {
            let client = self.inner.clone();

            tokio::spawn(async move {
                Controller::new(Api::<Secret>::all(client.clone()), ListParams::default())
                    .shutdown_on_signal()
                    .run(
                        reconcile_secret,
                        error_policy,
                        Arc::new(Data { client, state }),
                    )
                    .for_each(|res| async move {
                        if let Err(e) = res {
                            warn!("reconcile failed: {}", e)
                        }
                    })
                    .await;
            });
        }
    }
}

/// Controller triggers this whenever our main object or our children changed
async fn reconcile_source(source: Arc<Source>, ctx: Arc<Data>) -> Result<Action, Error> {
    match determine_action(&source) {
        BondAction::Create => {
            for s in &source.spec.secrets {
                let src_name = format!("{}/{}", source.metadata.namespace.clone().unwrap(), s.name);

                if ctx.state.read().await.contains_key(&src_name) {
                    continue;
                }

                let dst_names: Vec<String> = (&s.destinations)
                    .iter()
                    .map(|d| format!("{}/{}", d.namespace, d.name.as_ref().unwrap_or(&s.name)))
                    .collect();

                ctx.state.write().await.insert(src_name, dst_names);
            }

            let sources = Api::<Source>::namespaced(
                ctx.client.clone(),
                &source.metadata.namespace.clone().unwrap(),
            );

            let new_status = Patch::Merge(json!({
                "apiVersion": "bond.ectobit.com/v1alpha1",
                "metadata": {
                    "namespace": &source.metadata.namespace.clone().unwrap(),
                    "name": &source.metadata.name.clone().unwrap(),
                },
                "kind": "Source",
                "status": Status {
                    ready: format!("{}/{}", 0,ctx.state.read().await.len()),
                }
            }));

            if let Err(e) = sources
                .patch_status(
                    &source.metadata.name.clone().unwrap(),
                    &PatchParams::default(),
                    &new_status,
                )
                .await
            {
                error!("patch status: {}", e);
            }

            debug!("state: {:?}", ctx.state.read().await);

            let finalizer: Value = json!({
                "metadata": {
                    "finalizers": ["bind.ectobit.com"]
                }
            });
            let patch: Patch<&Value> = Patch::Merge(&finalizer);
            if let Err(e) = sources
                .patch(
                    source.metadata.name.as_ref().unwrap(),
                    &PatchParams::default(),
                    &patch,
                )
                .await
            {
                error!("finalizer: {}", e);
            }

            info!("source reconciled!");

            Ok(Action::requeue(Duration::from_secs(300)))
        }
        BondAction::Delete => {
            info!(">>>>>>>>>>>> DELETE");
            Ok(Action::requeue(Duration::from_secs(300)))
        }
        BondAction::NoOp => {
            info!(">>>>>>>>>>>> NOOP");
            Ok(Action::requeue(Duration::from_secs(300)))
        }
    }
}

/// Controller triggers this whenever our main object or our children changed
async fn reconcile_secret(secret: Arc<Secret>, ctx: Arc<Data>) -> Result<Action, Error> {
    let src_name = format!(
        "{}/{}",
        secret.metadata.namespace.clone().unwrap(),
        secret.metadata.name.clone().unwrap()
    );

    if ctx.state.read().await.contains_key(&src_name) {
        info!("found secret: {}", src_name);

        for dst_secrets in ctx.state.read().await.values() {
            for dst_secret in dst_secrets {
                let mut ds = dst_secret.split('/');
                let namespace = ds.next().unwrap().to_owned();
                let name = ds.next().unwrap().to_owned();

                let new_secret = Secret {
                    metadata: ObjectMeta {
                        namespace: Some(namespace.clone()),
                        name: Some(name.clone()),
                        ..ObjectMeta::default()
                    },
                    data: secret.data.clone(),
                    ..Default::default()
                };

                let sc = Api::<Secret>::namespaced(ctx.client.clone(), &namespace);
                match sc.create(&PostParams::default(), &new_secret).await {
                    Ok(_) => info!("secret {} reconciled to {}/{}", src_name, namespace, name),
                    Err(e) => error!("reconcile: {}", e),
                }
            }
        }
    }

    Ok(Action::requeue(Duration::from_secs(300)))
}

/// The controller triggers this on reconcile errors
fn error_policy(_error: &Error, _ctx: Arc<Data>) -> Action {
    Action::requeue(Duration::from_secs(60))
}

enum BondAction {
    Create,
    Delete,
    NoOp,
}

fn determine_action(source: &Source) -> BondAction {
    return if source.meta().deletion_timestamp.is_some() {
        BondAction::Delete
    } else if source.meta().finalizers.is_none() {
        BondAction::Create
    } else {
        BondAction::NoOp
    };
}

// Data we want access to in error/reconcile calls
struct Data {
    client: kube::Client,
    state: Arc<RwLock<HashMap<String, Vec<String>>>>,
}
