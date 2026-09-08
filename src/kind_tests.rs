//! Explicit live-cluster proof for the effect-gateway Deployment-image operation.

use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf};

use ed25519_dalek::SigningKey;
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec, ReplicaSet},
        core::v1::{Container, Namespace, Pod, PodSpec, PodTemplateSpec},
    },
    apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta},
};
use kube::{
    api::{Api, DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams},
    Client,
};
use serde_json::json;

use crate::{
    inspect_receipt, test_deployment_patch_document, ApprovedTarget, DeploymentImageAdapter,
    ExactAuthorization, FaultPoint, Gateway, GatewayError, InspectionLimits, InspectionStatus,
    KubernetesDeploymentImageAdapter, OperationResult, OperationState, ReceiptSettings,
    ReceiptTrust, SetDeploymentImageRequest, TargetIdentity,
};

const NAMESPACE: &str = "kapsel-effect-gateway";
const FAILED_NAMESPACE: &str = "kapsel-effect-gateway-failed";
const UNKNOWN_NAMESPACE: &str = "kapsel-effect-gateway-unknown";
const POLICY_NAMESPACE: &str = "kapsel-recovery-policy";
const SNAPSHOT_NAMESPACE: &str = "kapsel-effect-gateway-snapshot";
const DEPLOYMENT: &str = "image-demo";
const FAILED_DEPLOYMENT: &str = "image-demo-failed";
const UNKNOWN_DEPLOYMENT: &str = "image-demo-unknown";
const POLICY_DEPLOYMENT: &str = "image-demo-policy";
const TARGET_IMAGE: &str = concat!(
    "registry.k8s.io/pause@sha256:",
    "278fb9dbcca9518083ad1e11276933a2e96f23de604a3a08cc3c80002767d24c"
);
const FAILED_IMAGE: &str = concat!(
    "registry.example.invalid/kapsel/unhealthy@sha256:",
    "1111111111111111111111111111111111111111111111111111111111111111"
);
const FIXTURE_IMAGE: &str = "registry.k8s.io/pause:3.10.1";

struct CountingAdapter {
    inner: KubernetesDeploymentImageAdapter,
    apply_calls: usize,
}

impl CountingAdapter {
    fn new(client: Client) -> Self {
        Self {
            inner: KubernetesDeploymentImageAdapter::new(client),
            apply_calls: 0,
        }
    }
}

impl DeploymentImageAdapter for CountingAdapter {
    async fn identify(
        &mut self,
        request: &SetDeploymentImageRequest,
    ) -> Result<crate::TargetIdentity, crate::TargetReadError> {
        self.inner.identify(request).await
    }

    async fn apply(
        &mut self,
        permission: crate::gateway::DispatchPermission,
    ) -> Result<crate::ApplyOutcome, ()> {
        self.apply_calls += 1;
        self.inner.apply(permission).await
    }

    async fn observe(
        &mut self,
        request: &SetDeploymentImageRequest,
        outcome: &crate::ApplyOutcome,
    ) -> Result<crate::ReceiverObservation, ()> {
        self.inner.observe(request, outcome).await
    }
}

struct PreconditionRaceAdapter {
    client: Client,
    inner: KubernetesDeploymentImageAdapter,
    apply_calls: usize,
}

impl PreconditionRaceAdapter {
    fn new(client: Client) -> Self {
        Self {
            inner: KubernetesDeploymentImageAdapter::new(client.clone()),
            client,
            apply_calls: 0,
        }
    }
}

impl DeploymentImageAdapter for PreconditionRaceAdapter {
    async fn identify(
        &mut self,
        request: &SetDeploymentImageRequest,
    ) -> Result<crate::TargetIdentity, crate::TargetReadError> {
        self.inner.identify(request).await
    }

    async fn apply(
        &mut self,
        permission: crate::gateway::DispatchPermission,
    ) -> Result<crate::ApplyOutcome, ()> {
        let request = permission.request_for_test();
        self.apply_calls += 1;
        Api::<Deployment>::namespaced(self.client.clone(), &request.namespace)
            .patch(
                &request.deployment,
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata": {
                        "annotations": {
                            "kapsel.dev/kind-snapshot-race": request.operation_id,
                        },
                    },
                })),
            )
            .await
            .map_err(|_| ())?;
        self.inner.apply(permission).await
    }

    async fn observe(
        &mut self,
        request: &SetDeploymentImageRequest,
        outcome: &crate::ApplyOutcome,
    ) -> Result<crate::ReceiverObservation, ()> {
        self.inner.observe(request, outcome).await
    }
}

#[tokio::test]
#[ignore = "requires scripts/test-kind-effect-gateway.sh"]
async fn kind_changes_exactly_one_container_through_the_gateway() {
    assert_eq!(std::env::var("KAPSEL_KIND_TEST").as_deref(), Ok("1"));
    let client = Client::try_default().await.unwrap();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        namespaces.create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(NAMESPACE.into()),
                    ..ObjectMeta::default()
                },
                ..Namespace::default()
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let proof = tokio::time::timeout(
        std::time::Duration::from_mins(1),
        run_gateway_proof(client.clone()),
    )
    .await
    .map_or_else(
        |_| Err("kind gateway proof exceeded 60 seconds".into()),
        |result| result.map_err(|error| error.to_string()),
    );
    let cleanup = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        namespaces.delete(NAMESPACE, &DeleteParams::default()),
    )
    .await
    .map_or_else(
        |_| Err("kind cleanup exceeded 10 seconds".into()),
        |result| result.map(|_| ()).map_err(|error| error.to_string()),
    );
    assert!(cleanup.is_ok(), "kind fixture cleanup failed");
    proof.unwrap();
}

#[tokio::test]
#[ignore = "requires scripts/test-kind-effect-gateway.sh"]
async fn kind_failed_rollout_recovers_and_inspects_classifier_complete_receipt() {
    assert_eq!(std::env::var("KAPSEL_KIND_TEST").as_deref(), Ok("1"));
    let client = Client::try_default().await.unwrap();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        namespaces.create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(FAILED_NAMESPACE.into()),
                    ..ObjectMeta::default()
                },
                ..Namespace::default()
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let proof = tokio::time::timeout(
        std::time::Duration::from_mins(1),
        run_failed_rollout_proof(client.clone()),
    )
    .await
    .map_or_else(
        |_| Err("kind failed-rollout proof exceeded 60 seconds".into()),
        |result| result.map_err(|error| error.to_string()),
    );
    let cleanup = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        namespaces.delete(FAILED_NAMESPACE, &DeleteParams::default()),
    )
    .await
    .map_or_else(
        |_| Err("kind failed-rollout cleanup exceeded 10 seconds".into()),
        |result| result.map(|_| ()).map_err(|error| error.to_string()),
    );
    assert!(
        cleanup.is_ok(),
        "kind failed-rollout fixture cleanup failed"
    );
    proof.unwrap();
}

#[tokio::test]
#[ignore = "requires scripts/test-kind-effect-gateway.sh"]
async fn kind_deleted_after_patch_recovers_to_classifier_complete_unknown_receipt() {
    assert_eq!(std::env::var("KAPSEL_KIND_TEST").as_deref(), Ok("1"));
    let client = Client::try_default().await.unwrap();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        namespaces.create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(UNKNOWN_NAMESPACE.into()),
                    ..ObjectMeta::default()
                },
                ..Namespace::default()
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let proof = tokio::time::timeout(
        std::time::Duration::from_mins(1),
        run_unknown_rollout_proof(client.clone()),
    )
    .await
    .map_or_else(
        |_| Err("kind unknown proof exceeded 60 seconds".into()),
        |result| result.map_err(|error| error.to_string()),
    );
    let cleanup = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        namespaces.delete(UNKNOWN_NAMESPACE, &DeleteParams::default()),
    )
    .await
    .map_or_else(
        |_| Err("kind unknown cleanup exceeded 15 seconds".into()),
        |result| result.map(|_| ()).map_err(|error| error.to_string()),
    );
    assert!(cleanup.is_ok(), "kind unknown fixture cleanup failed");
    proof.unwrap();
}

#[tokio::test]
#[ignore = "requires scripts/test-kind-effect-gateway.sh"]
async fn kind_stale_exact_replay_reaches_admission_without_a_second_persisted_change() {
    assert_eq!(std::env::var("KAPSEL_KIND_TEST").as_deref(), Ok("1"));
    let client = Client::try_default().await.unwrap();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        namespaces.create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(POLICY_NAMESPACE.into()),
                    labels: Some(BTreeMap::from([(
                        "kapsel.dev/recovery-policy".into(),
                        "true".into(),
                    )])),
                    ..ObjectMeta::default()
                },
                ..Namespace::default()
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let proof = tokio::time::timeout(
        std::time::Duration::from_mins(1),
        run_recovery_policy_proof(client.clone()),
    )
    .await
    .map_or_else(
        |_| Err("kind recovery-policy proof exceeded 60 seconds".into()),
        |result| result.map_err(|error| error.to_string()),
    );
    let cleanup = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        namespaces.delete(POLICY_NAMESPACE, &DeleteParams::default()),
    )
    .await
    .map_or_else(
        |_| Err("kind recovery-policy cleanup exceeded 15 seconds".into()),
        |result| result.map(|_| ()).map_err(|error| error.to_string()),
    );
    assert!(cleanup.is_ok(), "kind recovery-policy cleanup failed");
    proof.unwrap();
}

#[tokio::test]
#[ignore = "requires scripts/test-kind-effect-gateway.sh"]
async fn kind_snapshot_approval_rejects_stale_targets_and_pins_the_conditional_patch() {
    assert_eq!(std::env::var("KAPSEL_KIND_TEST").as_deref(), Ok("1"));
    let client = Client::try_default().await.unwrap();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        namespaces.create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(SNAPSHOT_NAMESPACE.into()),
                    ..ObjectMeta::default()
                },
                ..Namespace::default()
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let proof = tokio::time::timeout(
        std::time::Duration::from_mins(1),
        run_snapshot_approval_proof(client.clone()),
    )
    .await
    .map_or_else(
        |_| Err("kind snapshot-approval proof exceeded 60 seconds".into()),
        |result| result.map_err(|error| error.to_string()),
    );
    let cleanup = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        namespaces.delete(SNAPSHOT_NAMESPACE, &DeleteParams::default()),
    )
    .await
    .map_or_else(
        |_| Err("kind snapshot-approval cleanup exceeded 15 seconds".into()),
        |result| result.map(|_| ()).map_err(|error| error.to_string()),
    );
    assert!(cleanup.is_ok(), "kind snapshot-approval cleanup failed");
    proof.unwrap();
}

async fn run_snapshot_approval_proof(client: Client) -> Result<(), Box<dyn std::error::Error>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), SNAPSHOT_NAMESPACE);

    prove_matching_snapshot_patches_once(&client, &deployments).await?;
    prove_drifted_snapshot_rejects_before_patch(&client, &deployments).await?;
    prove_recreated_snapshot_rejects_before_patch(&client, &deployments).await?;
    prove_precondition_race_remains_attempted(&client, &deployments).await?;
    Ok(())
}

async fn prove_matching_snapshot_patches_once(
    client: &Client,
    deployments: &Api<Deployment>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = snapshot_request("kind-snapshot-match-001", "snapshot-match");
    create_snapshot_fixture(deployments, &request.deployment).await?;
    let approval = snapshot_authorization(client, &request).await?;
    let directory = private_test_directory_for("snapshot-match");
    let database = directory.join("journal.sqlite3");
    let mut gateway = Gateway::open_for_test(&database)?;
    gateway.submit_exact_for_test(&request, &approval)?;
    let mut adapter = CountingAdapter::new(client.clone());

    assert_eq!(
        gateway.run_once_with_adapter(&mut adapter, None).await?,
        Some(OperationState::ReceiverObserved)
    );
    assert_eq!(adapter.apply_calls, 1);
    assert_eq!(
        gateway.result(&request.operation_id)?,
        Some(OperationResult::Succeeded)
    );
    drop(gateway);
    fs::remove_dir_all(directory)?;
    Ok(())
}

async fn prove_drifted_snapshot_rejects_before_patch(
    client: &Client,
    deployments: &Api<Deployment>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = snapshot_request("kind-snapshot-drift-001", "snapshot-drift");
    create_snapshot_fixture(deployments, &request.deployment).await?;
    let approval = snapshot_authorization(client, &request).await?;
    advance_deployment_version(deployments, &request.deployment, "drift").await?;
    let directory = private_test_directory_for("snapshot-drift");
    let database = directory.join("journal.sqlite3");
    let mut gateway = Gateway::open_for_test(&database)?;
    gateway.submit_exact_for_test(&request, &approval)?;
    let mut adapter = CountingAdapter::new(client.clone());

    assert_eq!(
        gateway.run_once_with_adapter(&mut adapter, None).await?,
        Some(OperationState::NotAttempted)
    );
    assert_eq!(adapter.apply_calls, 0);
    assert_eq!(gateway.result(&request.operation_id)?, None);
    drop(gateway);
    fs::remove_dir_all(directory)?;
    Ok(())
}

async fn prove_recreated_snapshot_rejects_before_patch(
    client: &Client,
    deployments: &Api<Deployment>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = snapshot_request("kind-snapshot-recreated-001", "snapshot-recreated");
    create_snapshot_fixture(deployments, &request.deployment).await?;
    let approval = snapshot_authorization(client, &request).await?;
    deployments
        .delete(&request.deployment, &DeleteParams::default())
        .await?;
    wait_for_deployment_deletion(deployments, &request.deployment).await?;
    create_snapshot_fixture(deployments, &request.deployment).await?;
    let directory = private_test_directory_for("snapshot-recreated");
    let database = directory.join("journal.sqlite3");
    let mut gateway = Gateway::open_for_test(&database)?;
    gateway.submit_exact_for_test(&request, &approval)?;
    let mut adapter = CountingAdapter::new(client.clone());

    assert_eq!(
        gateway.run_once_with_adapter(&mut adapter, None).await?,
        Some(OperationState::NotAttempted)
    );
    assert_eq!(adapter.apply_calls, 0);
    assert_eq!(gateway.result(&request.operation_id)?, None);
    drop(gateway);
    fs::remove_dir_all(directory)?;
    Ok(())
}

async fn prove_precondition_race_remains_attempted(
    client: &Client,
    deployments: &Api<Deployment>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = snapshot_request("kind-snapshot-race-001", "snapshot-race");
    create_snapshot_fixture(deployments, &request.deployment).await?;
    let approval = snapshot_authorization(client, &request).await?;
    let directory = private_test_directory_for("snapshot-race");
    let database = directory.join("journal.sqlite3");
    let mut gateway = Gateway::open_for_test(&database)?;
    gateway.submit_exact_for_test(&request, &approval)?;
    let mut adapter = PreconditionRaceAdapter::new(client.clone());

    assert!(matches!(
        gateway.run_once_with_adapter(&mut adapter, None).await,
        Err(GatewayError::KubernetesApply)
    ));
    assert_eq!(adapter.apply_calls, 1);
    assert_eq!(
        gateway.get(&request.operation_id)?,
        Some(OperationState::ApplyStarted)
    );
    assert_eq!(gateway.result(&request.operation_id)?, None);
    drop(gateway);
    fs::remove_dir_all(directory)?;
    Ok(())
}

async fn create_snapshot_fixture(
    deployments: &Api<Deployment>,
    deployment: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        deployments.create(
            &PostParams::default(),
            &fixture_deployment_for(SNAPSHOT_NAMESPACE, deployment),
        ),
    )
    .await??;
    wait_for_deployment_rollout(deployments, deployment).await
}

async fn snapshot_authorization(
    client: &Client,
    request: &SetDeploymentImageRequest,
) -> Result<ExactAuthorization, Box<dyn std::error::Error>> {
    let mut adapter = KubernetesDeploymentImageAdapter::new(client.clone());
    let target = adapter
        .identify(request)
        .await
        .map_err(|error| format!("could not acquire operator snapshot: {error:?}"))?;
    Ok(ExactAuthorization {
        approved_target: Some(ApprovedTarget {
            uid: target.deployment_uid,
            resource_version: target.resource_version,
        }),
        authorization_id: format!("{}-auth", request.operation_id),
        operation_id: request.operation_id.clone(),
        namespace: request.namespace.clone(),
        deployment: request.deployment.clone(),
        container: request.container.clone(),
        immutable_image_digest: request.immutable_image_digest.clone(),
    })
}

async fn advance_deployment_version(
    deployments: &Api<Deployment>,
    deployment: &str,
    annotation: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    deployments
        .patch(
            deployment,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {
                    "annotations": {
                        "kapsel.dev/kind-snapshot-test": annotation,
                    },
                },
            })),
        )
        .await?;
    Ok(())
}

async fn wait_for_deployment_deletion(
    deployments: &Api<Deployment>,
    deployment: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if deployments.get_opt(deployment).await?.is_none() {
                return Ok::<(), kube::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "deleted snapshot Deployment remained observable for 10 seconds")??;
    Ok(())
}

async fn run_recovery_policy_proof(client: Client) -> Result<(), Box<dyn std::error::Error>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), POLICY_NAMESPACE);
    let replica_sets: Api<ReplicaSet> = Api::namespaced(client.clone(), POLICY_NAMESPACE);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        deployments.create(
            &PostParams::default(),
            &fixture_deployment_for(POLICY_NAMESPACE, POLICY_DEPLOYMENT),
        ),
    )
    .await??;
    wait_for_deployment_rollout(&deployments, POLICY_DEPLOYMENT).await?;

    let request = policy_request();
    let mut adapter = KubernetesDeploymentImageAdapter::new(client.clone());
    let frozen_target = adapter.identify(&request).await.map_err(|error| {
        format!("could not freeze policy target before the first patch: {error:?}")
    })?;
    let before = deployments.get(POLICY_DEPLOYMENT).await?;
    let before_generation = before
        .metadata
        .generation
        .ok_or("missing initial generation")?;
    let selector = ListParams::default().labels("app=image-demo-policy");
    let replica_sets_before = replica_sets.list(&selector).await?.items.len();

    let first = adapter
        .apply(crate::gateway::dispatch_permission_for_test(
            &request,
            &frozen_target,
        ))
        .await
        .map_err(|()| "first frozen patch was rejected")?;
    assert_eq!(
        first.deployment_uid.as_deref(),
        Some(frozen_target.deployment_uid.as_str())
    );
    wait_for_deployment_rollout(&deployments, POLICY_DEPLOYMENT).await?;
    let after_first = deployments.get(POLICY_DEPLOYMENT).await?;
    let first_generation = after_first
        .metadata
        .generation
        .ok_or("missing generation after first patch")?;
    assert_eq!(first_generation, before_generation + 1);
    let replica_sets_after_first = replica_sets.list(&selector).await?.items.len();
    assert_eq!(replica_sets_after_first, replica_sets_before + 1);
    let admission_after_first = admission_effects(&client, "kind-policy-op-001").await?;
    assert_eq!(admission_after_first.len(), 1);

    assert_stale_replay_conflicts(&deployments, &request, &frozen_target).await?;
    let after_replay = deployments.get(POLICY_DEPLOYMENT).await?;
    assert_eq!(after_replay.metadata.uid, after_first.metadata.uid);
    assert_eq!(
        after_replay.metadata.resource_version,
        after_first.metadata.resource_version
    );
    assert_eq!(after_replay.metadata.generation, Some(first_generation));
    assert_eq!(after_replay.spec, after_first.spec);
    assert_eq!(
        deployment_container_image(&after_replay, "target"),
        Some(TARGET_IMAGE)
    );
    assert_eq!(
        deployment_container_image(&after_replay, "untouched"),
        Some(FIXTURE_IMAGE)
    );
    assert_eq!(
        after_replay
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("kapsel.dev/kap0038-operation-id"))
            .map(String::as_str),
        Some("kind-policy-op-001")
    );
    assert_eq!(
        replica_sets.list(&selector).await?.items.len(),
        replica_sets_after_first
    );
    let admission_after_replay =
        wait_for_admission_effects(&client, "kind-policy-op-001", 2).await?;
    assert_eq!(admission_after_replay.len(), 2);
    let admission_uids = admission_after_replay
        .iter()
        .filter_map(|line| line.split_once("uid=").map(|(_, value)| value))
        .filter_map(|value| value.split_once(' ').map(|(uid, _)| uid))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(admission_uids.len(), 2);
    report_recovery_policy_evidence(
        before_generation,
        first_generation,
        replica_sets_before,
        replica_sets_after_first,
        &admission_after_replay,
    );
    Ok(())
}

async fn assert_stale_replay_conflicts(
    deployments: &Api<Deployment>,
    request: &SetDeploymentImageRequest,
    target: &TargetIdentity,
) -> Result<(), Box<dyn std::error::Error>> {
    let replay = deployments
        .patch(
            POLICY_DEPLOYMENT,
            &PatchParams::default(),
            &Patch::Strategic(test_deployment_patch_document(request, target)),
        )
        .await;
    match replay {
        Err(kube::Error::Api(response)) if response.code == 409 => Ok(()),
        Err(kube::Error::Api(response)) => Err(format!(
            "stale replay returned Kubernetes API status {}",
            response.code
        )
        .into()),
        Err(error) => Err(format!("stale replay returned a non-API error: {error}").into()),
        Ok(_) => Err("stale replay unexpectedly persisted".into()),
    }
}

async fn admission_effects(
    client: &Client,
    operation_id: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), "kapsel-recovery-policy-webhook");
    let webhook_pods = pods
        .list(&ListParams::default().labels("app=recovery-policy-webhook"))
        .await?;
    let pod = webhook_pods.items.first().ok_or("missing webhook pod")?;
    let pod_name = pod
        .metadata
        .name
        .as_deref()
        .ok_or("webhook pod missing name")?;
    let logs = pods.logs(pod_name, &LogParams::default()).await?;
    Ok(logs
        .lines()
        .filter(|line| {
            line.contains("KAPSEL_ADMISSION_EFFECT")
                && line.contains(&format!("operation_id={operation_id}"))
        })
        .map(str::to_owned)
        .collect())
}

async fn wait_for_admission_effects(
    client: &Client,
    operation_id: &str,
    expected: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let effects = admission_effects(client, operation_id).await?;
            if effects.len() >= expected {
                return Ok::<Vec<String>, Box<dyn std::error::Error>>(effects);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "admission effect did not become observable within 10 seconds")?
}

fn deployment_container_image<'a>(
    deployment: &'a Deployment,
    container_name: &str,
) -> Option<&'a str> {
    deployment
        .spec
        .as_ref()?
        .template
        .spec
        .as_ref()?
        .containers
        .iter()
        .find(|container| container.name == container_name)?
        .image
        .as_deref()
}

#[allow(clippy::print_stdout)]
fn report_recovery_policy_evidence(
    before_generation: i64,
    after_generation: i64,
    replica_sets_before: usize,
    replica_sets_after: usize,
    admission_effects: &[String],
) {
    println!(
        "[kind recovery-policy] patch_requests=2 replay_status=409 admission_effects={} \
         persisted_deployment_changes=1 controller_effects=1 \
         generation={before_generation}->{after_generation} \
         replica_sets={replica_sets_before}->{replica_sets_after}",
        admission_effects.len()
    );
    for effect in admission_effects {
        println!("[kind recovery-policy] {effect}");
    }
}

async fn run_gateway_proof(client: Client) -> Result<(), Box<dyn std::error::Error>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), NAMESPACE);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        deployments.create(
            &PostParams::default(),
            &fixture_deployment_for(NAMESPACE, DEPLOYMENT),
        ),
    )
    .await??;
    wait_for_deployment_rollout(&deployments, DEPLOYMENT).await?;
    let request = request();
    let authorization = ExactAuthorization {
        approved_target: None,
        authorization_id: "kind-auth-001".into(),
        operation_id: request.operation_id.clone(),
        namespace: request.namespace.clone(),
        deployment: request.deployment.clone(),
        container: request.container.clone(),
        immutable_image_digest: request.immutable_image_digest.clone(),
    };
    let directory = private_test_directory_for("success");
    let database = directory.join("journal.sqlite3");
    let mut gateway = Gateway::open_for_test(&database)?;
    gateway.submit_exact_for_test(&request, &authorization)?;
    let mut adapter = KubernetesDeploymentImageAdapter::new(client.clone());
    match gateway
        .run_once_with_adapter(&mut adapter, Some(FaultPoint::ApplyReturned))
        .await
    {
        Err(GatewayError::InjectedFault) => {},
        Err(error) => return Err(error.into()),
        Ok(_) => return Err("kind fault injection did not stop after the patch".into()),
    }
    assert_eq!(
        gateway.get(&request.operation_id)?,
        Some(OperationState::ApplyStarted)
    );
    drop(gateway);
    let mut gateway = Gateway::open_for_test(&database)?;

    let state = gateway.run_once(client).await?;

    assert_eq!(state, Some(OperationState::ReceiverObserved));
    assert_eq!(
        gateway.result(&request.operation_id)?,
        Some(OperationResult::Succeeded)
    );
    let observed = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        deployments.get(DEPLOYMENT),
    )
    .await??;
    let containers = &observed
        .spec
        .as_ref()
        .and_then(|spec| spec.template.spec.as_ref())
        .ok_or("missing fixture pod spec")?
        .containers;
    assert_eq!(containers.len(), 2);
    assert_eq!(
        containers
            .iter()
            .find(|container| container.name == "target")
            .and_then(|container| container.image.as_deref()),
        Some(TARGET_IMAGE)
    );
    assert_eq!(
        containers
            .iter()
            .find(|container| container.name == "untouched")
            .and_then(|container| container.image.as_deref()),
        Some(FIXTURE_IMAGE)
    );
    drop(gateway);
    fs::remove_dir_all(directory)?;
    Ok(())
}

async fn run_unknown_rollout_proof(client: Client) -> Result<(), Box<dyn std::error::Error>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), UNKNOWN_NAMESPACE);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        deployments.create(
            &PostParams::default(),
            &fixture_deployment_for(UNKNOWN_NAMESPACE, UNKNOWN_DEPLOYMENT),
        ),
    )
    .await??;
    wait_for_deployment_rollout(&deployments, UNKNOWN_DEPLOYMENT).await?;
    let request = unknown_request();
    let authorization = ExactAuthorization {
        approved_target: None,
        authorization_id: "kind-unknown-auth-001".into(),
        operation_id: request.operation_id.clone(),
        namespace: request.namespace.clone(),
        deployment: request.deployment.clone(),
        container: request.container.clone(),
        immutable_image_digest: request.immutable_image_digest.clone(),
    };
    let directory = private_test_directory_for("unknown");
    let receipt_directory = directory.join("receipts");
    fs::create_dir(&receipt_directory)?;
    fs::set_permissions(&receipt_directory, fs::Permissions::from_mode(0o700))?;
    let database = directory.join("journal.sqlite3");
    let mut gateway = Gateway::open_for_test(&database)?;
    gateway.submit_exact_for_test(&request, &authorization)?;
    let mut first_adapter = CountingAdapter::new(client.clone());
    match gateway
        .run_once_with_adapter(&mut first_adapter, Some(FaultPoint::ApplyReturned))
        .await
    {
        Err(GatewayError::InjectedFault) => {},
        Err(error) => return Err(error.into()),
        Ok(_) => return Err("kind unknown fault did not stop after patch".into()),
    }
    assert_eq!(first_adapter.apply_calls, 1);
    drop(gateway);

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        deployments.delete(UNKNOWN_DEPLOYMENT, &DeleteParams::default()),
    )
    .await??;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if deployments.get_opt(UNKNOWN_DEPLOYMENT).await?.is_none() {
                return Ok::<(), kube::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "deleted Deployment remained observable for 10 seconds")??;

    let mut gateway = Gateway::open_for_test(&database)?;
    let mut recovery_adapter = CountingAdapter::new(client);
    assert_eq!(
        gateway
            .run_once_with_adapter(&mut recovery_adapter, None)
            .await?,
        Some(OperationState::ReceiverObserved)
    );
    assert_eq!(recovery_adapter.apply_calls, 0);
    assert_eq!(
        gateway.result(&request.operation_id)?,
        Some(OperationResult::Unknown)
    );

    let receipt_seed = [43_u8; 32];
    assert_eq!(
        gateway.finalize_receipt_once(&ReceiptSettings {
            signing_seed: &receipt_seed,
            key_id: "kind-unknown-receipt-key",
        })?,
        Some(OperationState::Finalized)
    );
    let (receipt_bytes, _) =
        Gateway::read_loaded_receipt(gateway.loaded_for_test(&request.operation_id)?.unwrap())?;
    let trust = ReceiptTrust {
        key_id: "kind-unknown-receipt-key".into(),
        public_key: SigningKey::from_bytes(&receipt_seed)
            .verifying_key()
            .to_bytes(),
        accepted_purpose: "kapsel.kap0038.kubernetes-effect-receipt.v2".into(),
        not_before_unix_s: 100,
        not_after_unix_s: 200,
    }
    .encode()?;
    let report = inspect_receipt(&receipt_bytes, &trust, 150, InspectionLimits::default());
    assert_eq!(report.status(), InspectionStatus::Inspected);
    let statement = report.statement().ok_or("missing inspected statement")?;
    assert_eq!(statement.result(), OperationResult::Unknown);
    assert_eq!(statement.observed_image(), None);
    assert_eq!(statement.observed_operation_marker(), None);
    drop(gateway);
    fs::remove_dir_all(directory)?;
    Ok(())
}

async fn run_failed_rollout_proof(client: Client) -> Result<(), Box<dyn std::error::Error>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), FAILED_NAMESPACE);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        deployments.create(
            &PostParams::default(),
            &fixture_deployment_for(FAILED_NAMESPACE, FAILED_DEPLOYMENT),
        ),
    )
    .await??;
    wait_for_deployment_rollout(&deployments, FAILED_DEPLOYMENT).await?;
    let request = failed_request();
    let authorization = ExactAuthorization {
        approved_target: None,
        authorization_id: "kind-failed-auth-001".into(),
        operation_id: request.operation_id.clone(),
        namespace: request.namespace.clone(),
        deployment: request.deployment.clone(),
        container: request.container.clone(),
        immutable_image_digest: request.immutable_image_digest.clone(),
    };
    let directory = private_test_directory_for("failed");
    let receipt_directory = directory.join("receipts");
    fs::create_dir(&receipt_directory)?;
    fs::set_permissions(&receipt_directory, fs::Permissions::from_mode(0o700))?;
    let database = directory.join("journal.sqlite3");
    let mut gateway = Gateway::open_for_test(&database)?;
    gateway.submit_exact_for_test(&request, &authorization)?;
    let mut first_adapter = CountingAdapter::new(client.clone());
    match gateway
        .run_once_with_adapter(&mut first_adapter, Some(FaultPoint::ApplyReturned))
        .await
    {
        Err(GatewayError::InjectedFault) => {},
        Err(error) => return Err(error.into()),
        Ok(_) => return Err("kind failed-rollout fault did not stop after patch".into()),
    }
    assert_eq!(first_adapter.apply_calls, 1);
    drop(gateway);

    let mut gateway = Gateway::open_for_test(&database)?;
    let mut recovery_adapter = CountingAdapter::new(client);
    assert_eq!(
        gateway
            .run_once_with_adapter(&mut recovery_adapter, None)
            .await?,
        Some(OperationState::ReceiverObserved)
    );
    assert_eq!(recovery_adapter.apply_calls, 0);
    assert_eq!(
        gateway.result(&request.operation_id)?,
        Some(OperationResult::Failed)
    );

    let receipt_seed = [41_u8; 32];
    assert_eq!(
        gateway.finalize_receipt_once(&ReceiptSettings {
            signing_seed: &receipt_seed,
            key_id: "kind-failed-receipt-key",
        })?,
        Some(OperationState::Finalized)
    );
    let (receipt_bytes, _) =
        Gateway::read_loaded_receipt(gateway.loaded_for_test(&request.operation_id)?.unwrap())?;
    let trust = ReceiptTrust {
        key_id: "kind-failed-receipt-key".into(),
        public_key: SigningKey::from_bytes(&receipt_seed)
            .verifying_key()
            .to_bytes(),
        accepted_purpose: "kapsel.kap0038.kubernetes-effect-receipt.v2".into(),
        not_before_unix_s: 100,
        not_after_unix_s: 200,
    }
    .encode()?;
    let report = inspect_receipt(&receipt_bytes, &trust, 150, InspectionLimits::default());
    assert_eq!(report.status(), InspectionStatus::Inspected);
    let statement = report.statement().ok_or("missing inspected statement")?;
    assert_eq!(statement.result(), OperationResult::Failed);
    assert_eq!(
        statement.rollout_condition_reason(),
        Some("ProgressDeadlineExceeded")
    );
    assert_eq!(statement.observed_image(), Some(FAILED_IMAGE));
    assert_eq!(
        statement.observed_operation_marker(),
        Some("kind-failed-op-001")
    );
    drop(gateway);
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[allow(clippy::print_stdout)]
async fn wait_for_deployment_rollout(
    deployments: &Api<Deployment>,
    deployment_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let deployment = deployments.get(deployment_name).await?;
            let generation = deployment.metadata.generation;
            let ready = deployment.status.as_ref().is_some_and(|status| {
                status.observed_generation == generation
                    && status.available_replicas == Some(1)
                    && status.updated_replicas == Some(1)
                    && status.replicas == Some(1)
                    && status.ready_replicas == Some(1)
            });
            if ready {
                return Ok::<(), kube::Error>(());
            }
            println!("waiting for the disposable kind fixture rollout");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
    .await
    .map_err(|_| "fixture rollout exceeded 30 seconds")??;
    Ok(())
}

fn request() -> SetDeploymentImageRequest {
    SetDeploymentImageRequest {
        operation_id: "kind-op-001".into(),
        namespace: NAMESPACE.into(),
        deployment: DEPLOYMENT.into(),
        container: "target".into(),
        immutable_image_digest: TARGET_IMAGE.into(),
    }
}

fn unknown_request() -> SetDeploymentImageRequest {
    SetDeploymentImageRequest {
        operation_id: "kind-unknown-op-001".into(),
        namespace: UNKNOWN_NAMESPACE.into(),
        deployment: UNKNOWN_DEPLOYMENT.into(),
        container: "target".into(),
        immutable_image_digest: TARGET_IMAGE.into(),
    }
}

fn policy_request() -> SetDeploymentImageRequest {
    SetDeploymentImageRequest {
        operation_id: "kind-policy-op-001".into(),
        namespace: POLICY_NAMESPACE.into(),
        deployment: POLICY_DEPLOYMENT.into(),
        container: "target".into(),
        immutable_image_digest: TARGET_IMAGE.into(),
    }
}

fn snapshot_request(operation_id: &str, deployment: &str) -> SetDeploymentImageRequest {
    SetDeploymentImageRequest {
        operation_id: operation_id.into(),
        namespace: SNAPSHOT_NAMESPACE.into(),
        deployment: deployment.into(),
        container: "target".into(),
        immutable_image_digest: TARGET_IMAGE.into(),
    }
}

fn failed_request() -> SetDeploymentImageRequest {
    SetDeploymentImageRequest {
        operation_id: "kind-failed-op-001".into(),
        namespace: FAILED_NAMESPACE.into(),
        deployment: FAILED_DEPLOYMENT.into(),
        container: "target".into(),
        immutable_image_digest: FAILED_IMAGE.into(),
    }
}

fn fixture_deployment_for(namespace: &str, deployment: &str) -> Deployment {
    let labels = BTreeMap::from([("app".into(), deployment.into())]);
    Deployment {
        metadata: ObjectMeta {
            name: Some(deployment.into()),
            namespace: Some(namespace.into()),
            ..ObjectMeta::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(1),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..LabelSelector::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..ObjectMeta::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![
                        Container {
                            name: "target".into(),
                            image: Some(FIXTURE_IMAGE.into()),
                            ..Container::default()
                        },
                        Container {
                            name: "untouched".into(),
                            image: Some(FIXTURE_IMAGE.into()),
                            ..Container::default()
                        },
                    ],
                    ..PodSpec::default()
                }),
            },
            progress_deadline_seconds: Some(15),
            ..DeploymentSpec::default()
        }),
        ..Deployment::default()
    }
}

fn private_test_directory_for(scenario: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kapsel-kind-proof-{}-{scenario}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    fs::canonicalize(path).unwrap()
}

// Deliberately bypasses dispatch permission only in this receiver experiment. No
// experimental patch or replay path is available to the production gateway.
mod patch_experiment {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use serde_json::Value;

    use super::*;

    const EXPERIMENT_NAMESPACE: &str = "kapsel-patch-experiment";
    const MARKER: &str = "kapsel.dev/kap0038-operation-id";
    type ExperimentResult<T> = Result<T, Box<dyn std::error::Error>>;

    #[derive(Clone, Copy, Debug)]
    enum Strategy {
        Strategic,
        Json,
    }

    impl Strategy {
        fn content_type(self) -> &'static str {
            match self {
                Self::Strategic => "application/strategic-merge-patch+json",
                Self::Json => "application/json-patch+json",
            }
        }

        fn stale_status(self) -> u16 {
            match self {
                Self::Strategic => 409,
                Self::Json => 422,
            }
        }
    }

    // Freeze the index and annotations from the ORIGINAL approved snapshot.
    // Adding a member requires an existing parent (RFC 6902 section 4.1).
    // Add the whole preserved map only when the snapshot has no annotations.
    fn json_document(request: &SetDeploymentImageRequest, original: &Deployment) -> Value {
        let containers = &original
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers;
        let index = containers
            .iter()
            .position(|c| c.name == request.container)
            .unwrap();
        let mut operations = vec![
            json!({"op": "test", "path": "/metadata/uid", "value": original.metadata.uid}),
            json!({"op": "test", "path": "/metadata/resourceVersion",
                   "value": original.metadata.resource_version}),
            json!({"op": "test", "path": format!("/spec/template/spec/containers/{index}/name"),
                   "value": request.container}),
            json!({"op": "replace", "path": format!("/spec/template/spec/containers/{index}/image"),
                   "value": request.immutable_image_digest}),
        ];
        if original.metadata.annotations.is_some() {
            operations.push(json!({"op": "add",
                "path": "/metadata/annotations/kapsel.dev~1kap0038-operation-id",
                "value": request.operation_id}));
        } else {
            operations.push(json!({"op": "add", "path": "/metadata/annotations",
                "value": {MARKER: request.operation_id}}));
        }
        Value::Array(operations)
    }

    struct FrozenPatch {
        strategy: Strategy,
        request: SetDeploymentImageRequest,
        original: Deployment,
        bytes: Vec<u8>,
        sends: Arc<AtomicUsize>,
    }

    impl FrozenPatch {
        fn new(
            strategy: Strategy,
            request: SetDeploymentImageRequest,
            original: Deployment,
        ) -> Self {
            let target = TargetIdentity {
                deployment_uid: original.metadata.uid.clone().unwrap(),
                resource_version: original.metadata.resource_version.clone().unwrap(),
            };
            let document = match strategy {
                Strategy::Strategic => test_deployment_patch_document(&request, &target),
                Strategy::Json => json_document(&request, &original),
            };
            Self {
                strategy,
                request,
                original,
                bytes: serde_json::to_vec(&document).unwrap(),
                sends: Arc::new(AtomicUsize::new(0)),
            }
        }

        async fn send(&self, client: &Client) -> Result<u16, kube::Error> {
            let request = http::Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/apis/apps/v1/namespaces/{}/deployments/{}",
                    self.request.namespace, self.request.deployment
                ))
                .header("content-type", self.strategy.content_type())
                .header("user-agent", "kapsel-frozen-patch-experiment")
                .body(self.bytes.clone())
                .unwrap();
            self.sends.fetch_add(1, Ordering::SeqCst);
            match client.request::<Deployment>(request).await {
                Ok(_) => Ok(200),
                Err(kube::Error::Api(response)) => Ok(response.code),
                Err(error) => Err(error),
            }
        }
    }

    async fn control(
        client: &Client,
        operation_id: &str,
        action: Value,
    ) -> ExperimentResult<Value> {
        let mut command = action;
        command["operation_id"] = json!(operation_id);
        let request = http::Request::builder()
            .method("POST")
            .uri(concat!(
                "/api/v1/namespaces/kapsel-recovery-policy-webhook/services/",
                "http:recovery-policy-webhook:control/proxy/control"
            ))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&command)?)
            .unwrap();
        Ok(client.request(request).await?)
    }

    async fn wait_at_barrier(
        client: &Client,
        operation_id: &str,
        count: usize,
    ) -> ExperimentResult<()> {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let state = control(client, operation_id, json!({"action": "state"})).await?;
                if state["effects"].as_array().unwrap().len() >= count {
                    assert_eq!(state["released"], 0);
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
                // Polling cadence is not ordering evidence. The held invocation ledger is.
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| "admission barrier was not reached in 10 seconds")?
    }

    #[tokio::test]
    #[ignore = "requires scripts/test-kind-effect-gateway.sh"]
    async fn kind_frozen_patch_receiver_matrix() {
        assert_eq!(std::env::var("KAPSEL_KIND_TEST").as_deref(), Ok("1"));
        let mut config = kube::Config::infer().await.unwrap();
        config.default_retry = false;
        let client = Client::try_from(config).unwrap();
        let namespaces: Api<Namespace> = Api::all(client.clone());
        namespaces
            .create(
                &PostParams::default(),
                &Namespace {
                    metadata: ObjectMeta {
                        name: Some(EXPERIMENT_NAMESPACE.into()),
                        labels: Some(BTreeMap::from([(
                            "kapsel.dev/recovery-policy".into(),
                            "true".into(),
                        )])),
                        ..ObjectMeta::default()
                    },
                    ..Namespace::default()
                },
            )
            .await
            .unwrap();
        let proof = tokio::time::timeout(std::time::Duration::from_secs(240), async {
            for strategy in [Strategy::Strategic, Strategy::Json] {
                for scenario in [
                    "persisted-discard",
                    "preflight-writer",
                    "recreated",
                    "reordered",
                    "overlap",
                    "unpersisted-replay",
                    "before-send",
                ] {
                    Box::pin(run_case(&client, strategy, scenario)).await?;
                }
            }
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            namespaces.delete(EXPERIMENT_NAMESPACE, &DeleteParams::default()),
        )
        .await
        .unwrap()
        .unwrap();
        proof.unwrap().unwrap();
    }

    async fn run_case(client: &Client, strategy: Strategy, scenario: &str) -> ExperimentResult<()> {
        let name = format!(
            "{}-{scenario}",
            match strategy {
                Strategy::Strategic => "strategic",
                Strategy::Json => "json",
            }
        );
        let deployments: Api<Deployment> = Api::namespaced(client.clone(), EXPERIMENT_NAMESPACE);
        let mut fixture = fixture_deployment_for(EXPERIMENT_NAMESPACE, &name);
        fixture.metadata.annotations = Some(BTreeMap::from([(
            "kapsel.dev/experiment-preserve".into(),
            "original".into(),
        )]));
        deployments.create(&PostParams::default(), &fixture).await?;
        wait_for_deployment_rollout(&deployments, &name).await?;
        let request = SetDeploymentImageRequest {
            operation_id: name.clone(),
            namespace: EXPERIMENT_NAMESPACE.into(),
            deployment: name.clone(),
            container: "target".into(),
            immutable_image_digest: TARGET_IMAGE.into(),
        };
        let original = deployments.get(&name).await?;
        let mut adapter = KubernetesDeploymentImageAdapter::new(client.clone());
        let preflight = adapter.identify(&request).await.unwrap();
        assert_eq!(Some(preflight.deployment_uid), original.metadata.uid);
        assert_eq!(
            Some(preflight.resource_version),
            original.metadata.resource_version
        );
        let frozen = Arc::new(FrozenPatch::new(strategy, request, original));
        control(
            client,
            &name,
            json!({"action": "configure", "hold": scenario == "overlap",
            "invalidate_first": scenario == "unpersisted-replay"}),
        )
        .await?;
        let replica_sets: Api<ReplicaSet> = Api::namespaced(client.clone(), EXPERIMENT_NAMESPACE);
        let selector = ListParams::default().labels(&format!("app={name}"));
        let rs_before = replica_sets.list(&selector).await?.items.len();
        let statuses = exercise_scenario(client, &deployments, &frozen, scenario).await?;
        wait_for_deployment_rollout(&deployments, &name).await?;
        let after = deployments.get(&name).await?;
        let rs_after = replica_sets.list(&selector).await?.items.len();
        let state = control(client, &name, json!({"action": "state"})).await?;
        let invocations = state["invocations"].as_array().unwrap();
        let effects = wait_for_admission_effects(client, &name, invocations.len()).await?;
        assert_eq!(state["effects"].as_array().unwrap().len(), effects.len());
        assert_eq!(invocations.len(), effects.len());
        for uid in invocations {
            assert!(effects
                .iter()
                .any(|line| line.contains(uid.as_str().unwrap())));
        }
        let expected_effects = match scenario {
            "overlap" | "unpersisted-replay" => 2,
            "persisted-discard" => match strategy {
                Strategy::Strategic => 2,
                Strategy::Json => 1,
            },
            "before-send" => 1,
            "preflight-writer" | "reordered" | "recreated" => match strategy {
                Strategy::Strategic => 1,
                Strategy::Json => 0,
            },
            _ => unreachable!(),
        };
        // GuaranteedUpdate may re-enter admission within one strategic request.
        // The receiver ledger, not a request-count assumption, owns the measurement.
        if matches!(strategy, Strategy::Strategic) {
            assert!(effects.len() >= expected_effects);
        } else if expected_effects == 0 {
            // A stale cached original can pass JSON tests before the storage CAS
            // notices the writer. Tests against the refreshed object then fail.
            assert!(effects.len() <= 1);
        } else {
            assert_eq!(effects.len(), expected_effects);
        }
        if statuses.contains(&200) {
            assert_persisted_image_change(&frozen, &after);
            assert_eq!(rs_after, rs_before + 1);
        }
        report_case(&frozen, &statuses, &after, &effects, rs_before, rs_after);
        Ok(())
    }

    fn assert_persisted_image_change(frozen: &FrozenPatch, after: &Deployment) {
        assert_eq!(after.metadata.uid, frozen.original.metadata.uid);
        let mut expected_spec = frozen.original.spec.clone().unwrap();
        let container = expected_spec
            .template
            .spec
            .as_mut()
            .unwrap()
            .containers
            .iter_mut()
            .find(|container| container.name == frozen.request.container)
            .unwrap();
        container.image = Some(frozen.request.immutable_image_digest.clone());
        assert_eq!(after.spec.as_ref(), Some(&expected_spec));
        for (key, value) in frozen.original.metadata.annotations.as_ref().unwrap() {
            if key != MARKER && key != "deployment.kubernetes.io/revision" {
                assert_eq!(
                    after.metadata.annotations.as_ref().unwrap().get(key),
                    Some(value)
                );
            }
        }
        assert_eq!(
            after.metadata.generation.unwrap(),
            frozen.original.metadata.generation.unwrap() + 1
        );
        assert_eq!(
            after.metadata.annotations.as_ref().unwrap().get(MARKER),
            Some(&frozen.request.operation_id)
        );
    }

    async fn exercise_scenario(
        client: &Client,
        deployments: &Api<Deployment>,
        frozen: &Arc<FrozenPatch>,
        scenario: &str,
    ) -> ExperimentResult<Vec<u16>> {
        let name = &frozen.request.deployment;
        let strategy = frozen.strategy;
        let statuses = match scenario {
            "persisted-discard" => {
                // Harness sees persistence, then deliberately discards the response for
                // caller semantics. This is not TCP loss or a process-kill injection.
                assert_eq!(frozen.send(client).await?, 200);
                wait_for_deployment_rollout(deployments, name).await?;
                let after_first = deployments.get(name).await?;
                let replay = frozen.send(client).await?;
                assert_eq!(replay, strategy.stale_status());
                let after_replay = deployments.get(name).await?;
                assert_eq!(after_first.spec, after_replay.spec);
                assert_eq!(
                    after_first.metadata.generation,
                    after_replay.metadata.generation
                );
                vec![200, replay]
            },
            "preflight-writer" | "recreated" | "reordered" => {
                change_before_send(deployments, name, scenario).await?;
                let before_send = deployments.get(name).await?;
                if scenario == "recreated" {
                    assert_ne!(before_send.metadata.uid, frozen.original.metadata.uid);
                }
                let status = frozen.send(client).await?;
                assert_eq!(status, strategy.stale_status());
                let after = deployments.get(name).await?;
                assert_eq!(before_send.metadata.uid, after.metadata.uid);
                assert_eq!(before_send.metadata.generation, after.metadata.generation);
                assert_eq!(before_send.spec, after.spec);
                assert_eq!(before_send.metadata.annotations, after.metadata.annotations);
                vec![status]
            },
            "overlap" => overlap(client, deployments, frozen).await?,
            "unpersisted-replay" => {
                assert_eq!(frozen.send(client).await?, 422);
                let unpersisted = deployments.get(name).await?;
                assert_eq!(
                    unpersisted.metadata.resource_version,
                    frozen.original.metadata.resource_version
                );
                assert_eq!(unpersisted.spec, frozen.original.spec);
                wait_for_admission_effects(client, name, 1).await?;
                control(client, name, json!({"action": "allow"})).await?;
                assert_eq!(frozen.send(client).await?, 200);
                vec![422, 200]
            },
            "before-send" => {
                prove_before_send_fault(client, frozen).await?;
                assert_eq!(frozen.sends.load(Ordering::SeqCst), 0);
                assert!(admission_effects(client, name).await?.is_empty());
                assert_eq!(deployments.get(name).await?.spec, frozen.original.spec);
                report_unsent(name);
                // Counterfactual replay, NOT gateway recovery. Frozen original bytes.
                assert_eq!(frozen.send(client).await?, 200);
                vec![200]
            },
            _ => unreachable!(),
        };
        Ok(statuses)
    }

    async fn change_before_send(
        deployments: &Api<Deployment>,
        name: &str,
        scenario: &str,
    ) -> ExperimentResult<()> {
        match scenario {
            "preflight-writer" => advance_deployment_version(deployments, name, "writer").await?,
            "recreated" => {
                deployments.delete(name, &DeleteParams::default()).await?;
                wait_for_deployment_deletion(deployments, name).await?;
                deployments
                    .create(
                        &PostParams::default(),
                        &fixture_deployment_for(EXPERIMENT_NAMESPACE, name),
                    )
                    .await?;
                wait_for_deployment_rollout(deployments, name).await?;
            },
            "reordered" => {
                let mut changed = deployments.get(name).await?;
                changed
                    .spec
                    .as_mut()
                    .unwrap()
                    .template
                    .spec
                    .as_mut()
                    .unwrap()
                    .containers
                    .swap(0, 1);
                deployments
                    .replace(name, &PostParams::default(), &changed)
                    .await?;
                wait_for_deployment_rollout(deployments, name).await?;
            },
            _ => unreachable!(),
        }
        Ok(())
    }

    async fn overlap(
        client: &Client,
        deployments: &Api<Deployment>,
        frozen: &Arc<FrozenPatch>,
    ) -> ExperimentResult<Vec<u16>> {
        let name = &frozen.request.operation_id;
        let first = spawn_send(client, frozen);
        wait_at_barrier(client, name, 1).await?;
        let second = spawn_send(client, frozen);
        wait_at_barrier(client, name, 2).await?;
        assert!(!first.is_finished());
        assert!(!second.is_finished());
        let held = deployments.get(&frozen.request.deployment).await?;
        assert_eq!(
            held.metadata.resource_version,
            frozen.original.metadata.resource_version
        );
        assert_eq!(held.spec, frozen.original.spec);
        report_held(
            name,
            frozen.sends.load(Ordering::SeqCst),
            wait_for_admission_effects(client, name, 2).await?.len(),
        );
        control(client, name, json!({"action": "release", "through": 1})).await?;
        assert_eq!(first.await??, 200);
        control(client, name, json!({"action": "release", "through": 16})).await?;
        let second_status = second.await??;
        assert_eq!(second_status, frozen.strategy.stale_status());
        Ok(vec![200, second_status])
    }

    fn spawn_send(
        client: &Client,
        frozen: &Arc<FrozenPatch>,
    ) -> tokio::task::JoinHandle<Result<u16, kube::Error>> {
        let client = client.clone();
        let frozen = Arc::clone(frozen);
        tokio::spawn(async move { frozen.send(&client).await })
    }

    async fn prove_before_send_fault(
        client: &Client,
        frozen: &FrozenPatch,
    ) -> ExperimentResult<()> {
        let directory = private_test_directory_for(&frozen.request.operation_id);
        let database = directory.join("journal.sqlite3");
        let mut gateway = Gateway::open_for_test(&database)?;
        let authorization = ExactAuthorization {
            approved_target: Some(ApprovedTarget {
                uid: frozen.original.metadata.uid.clone().unwrap(),
                resource_version: frozen.original.metadata.resource_version.clone().unwrap(),
            }),
            authorization_id: frozen.request.operation_id.clone(),
            operation_id: frozen.request.operation_id.clone(),
            namespace: frozen.request.namespace.clone(),
            deployment: frozen.request.deployment.clone(),
            container: frozen.request.container.clone(),
            immutable_image_digest: frozen.request.immutable_image_digest.clone(),
        };
        gateway.submit_exact_for_test(&frozen.request, &authorization)?;
        let mut adapter = CountingAdapter::new(client.clone());
        assert!(matches!(
            gateway
                .run_once_with_adapter(&mut adapter, Some(FaultPoint::ApplyStartedCommitted))
                .await,
            Err(GatewayError::InjectedFault)
        ));
        assert_eq!(adapter.apply_calls, 0);
        drop(gateway);
        let reopened = Gateway::open_for_test(&database)?;
        assert_eq!(
            reopened.get(&frozen.request.operation_id)?,
            Some(OperationState::ApplyStarted)
        );
        assert_eq!(reopened.result(&frozen.request.operation_id)?, None);
        drop(reopened);
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[allow(clippy::print_stdout)]
    fn report_unsent(name: &str) {
        println!(
            "[patch comparison] {name} fault=ApplyStartedCommitted-injected-return-and-reopen \
            dispatch_calls=0 admission_effects=0 persisted_changes=0 caller_result=none"
        );
    }

    #[allow(clippy::print_stdout)]
    fn report_held(name: &str, sends: usize, effects: usize) {
        println!(
            "[patch comparison] {name} barrier=both-pending dispatch_calls={sends} \
            admission_effects={effects} persisted_changes=0 resource_version=original \
            caller_result=none"
        );
    }

    #[allow(clippy::print_stdout)]
    fn report_case(
        frozen: &FrozenPatch,
        statuses: &[u16],
        after: &Deployment,
        effects: &[String],
        rs_before: usize,
        rs_after: usize,
    ) {
        println!(
            "[patch comparison] {} strategy={:?} dispatch_calls={} http_statuses={statuses:?} \
            admission_invocations={} log_effects={} desired_spec_changed={} \
            generation={:?}->{:?} replica_sets={rs_before}->{rs_after} \
            uid={:?}->{:?} resource_version={:?}->{:?} \
            observed_generation={:?} available_replicas={:?} updated_replicas={:?} \
            caller_result=not-classified",
            frozen.request.operation_id,
            frozen.strategy,
            frozen.sends.load(Ordering::SeqCst),
            effects.len(),
            effects.len(),
            after.spec != frozen.original.spec,
            frozen.original.metadata.generation,
            after.metadata.generation,
            frozen.original.metadata.uid,
            after.metadata.uid,
            frozen.original.metadata.resource_version,
            after.metadata.resource_version,
            after.status.as_ref().and_then(|s| s.observed_generation),
            after.status.as_ref().and_then(|s| s.available_replicas),
            after.status.as_ref().and_then(|s| s.updated_replicas)
        );
        for effect in effects {
            println!("[patch comparison] {effect}");
        }
    }

    #[test]
    fn frozen_json_document_tests_original_identity_and_index_before_any_write() {
        let mut original = fixture_deployment_for(EXPERIMENT_NAMESPACE, "document");
        original.metadata.uid = Some("original-uid".into());
        original.metadata.resource_version = Some("opaque-original-rv".into());
        let request = request();
        let no_annotations = json_document(&request, &original);
        assert_eq!(
            no_annotations[0],
            json!({"op": "test", "path": "/metadata/uid",
            "value": "original-uid"})
        );
        assert_eq!(
            no_annotations[1],
            json!({"op": "test", "path": "/metadata/resourceVersion",
            "value": "opaque-original-rv"})
        );
        assert_eq!(
            no_annotations[2],
            json!({"op": "test",
            "path": "/spec/template/spec/containers/0/name", "value": "target"})
        );
        assert_eq!(no_annotations[3]["op"], "replace");
        assert_eq!(no_annotations[4]["path"], "/metadata/annotations");
        original.metadata.annotations = Some(BTreeMap::from([("keep".into(), "me".into())]));
        original
            .spec
            .as_mut()
            .unwrap()
            .template
            .spec
            .as_mut()
            .unwrap()
            .containers
            .swap(0, 1);
        let reordered = json_document(&request, &original);
        assert_eq!(
            reordered[2]["path"],
            "/spec/template/spec/containers/1/name"
        );
        assert_eq!(
            reordered[3]["path"],
            "/spec/template/spec/containers/1/image"
        );
        assert_eq!(
            reordered[4]["path"],
            "/metadata/annotations/kapsel.dev~1kap0038-operation-id"
        );
        assert_eq!(
            no_annotations[2]["path"],
            "/spec/template/spec/containers/0/name"
        );
    }
}
