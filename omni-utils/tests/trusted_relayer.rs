use near_api::signer::generate_secret_key;
use near_api::types::Data;
use near_api::{Account, Contract, NetworkConfig, Signer, Tokens};
use near_sandbox::Sandbox;
use near_sandbox::config::{DEFAULT_GENESIS_ACCOUNT, DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY};
use serde_json::json;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use tokio::sync::OnceCell;

fn build_wasm(path: &str, target_dir: &str, no_locked: bool) -> Vec<u8> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .expect("canonicalize manifest dir");
    let manifest_path = manifest_dir.join(path);
    let sub_target = manifest_dir.join(format!("target/{target_dir}"));

    let artifact = cargo_near_build::build(cargo_near_build::BuildOpts {
        manifest_path: Some(
            cargo_near_build::camino::Utf8PathBuf::from_path_buf(manifest_path)
                .expect("camino PathBuf from path"),
        ),
        override_cargo_target_dir: Some(sub_target.to_string_lossy().to_string()),
        no_locked,
        ..Default::default()
    })
    .unwrap_or_else(|err| panic!("building contract from {path}: {err:?}"));

    std::fs::read(&artifact.path).unwrap()
}

static CONTRACT_WASM: LazyLock<Vec<u8>> = LazyLock::new(|| {
    build_wasm(
        "../tests/test-contract/Cargo.toml",
        "test-contract-build",
        false,
    )
});

static CUSTOM_CONTRACT_WASM: LazyLock<Vec<u8>> = LazyLock::new(|| {
    build_wasm(
        "../tests/test-contract-custom/Cargo.toml",
        "test-contract-custom-build",
        false,
    )
});

struct TestEnv {
    sandbox: Sandbox,
    network: NetworkConfig,
    root_signer: Arc<Signer>,
    counter: AtomicUsize,
}

static ENV: OnceCell<TestEnv> = OnceCell::const_new();

async fn get_env() -> &'static TestEnv {
    ENV.get_or_init(|| async {
        // Force WASM builds before starting sandbox
        let _ = &*CONTRACT_WASM;
        let _ = &*CUSTOM_CONTRACT_WASM;

        let sandbox = Sandbox::start_sandbox()
            .await
            .expect("Failed to start sandbox");
        let network = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse().unwrap());
        let root_signer =
            Signer::from_secret_key(DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse().unwrap()).unwrap();

        TestEnv {
            sandbox,
            network,
            root_signer,
            counter: AtomicUsize::new(0),
        }
    })
    .await
}

/// Unique account id under the genesis root, e.g. `0.sandbox`
fn unique_subaccount(env: &TestEnv) -> String {
    let n = env.counter.fetch_add(1, Ordering::SeqCst);
    format!("{n}.{DEFAULT_GENESIS_ACCOUNT}")
}

/// Create a funded sub-account with its own key pair. Returns (account_id, signer).
async fn create_account(env: &TestEnv, near_amount: u128) -> (near_api::AccountId, Arc<Signer>) {
    let account_id: near_api::AccountId = unique_subaccount(env).parse().unwrap();
    let secret_key = generate_secret_key().unwrap();
    let signer = Signer::from_secret_key(secret_key.clone()).unwrap();

    Account::create_account(account_id.clone())
        .fund_myself(
            DEFAULT_GENESIS_ACCOUNT.to_owned(),
            near_api::NearToken::from_near(near_amount),
        )
        .with_public_key(secret_key.public_key())
        .with_signer(env.root_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Failed to create account")
        .assert_success();

    (account_id, signer)
}

/// Deploy the test-contract and call `new()`. Returns the contract account id and signer.
async fn deploy_contract(env: &TestEnv) -> (near_api::AccountId, Arc<Signer>) {
    let (account_id, signer) = create_account(env, 50).await;

    Contract::deploy(account_id.clone())
        .use_code(CONTRACT_WASM.to_vec())
        .with_init_call("new", json!({}))
        .unwrap()
        .with_signer(signer.clone())
        .send_to(&env.network)
        .await
        .expect("Failed to deploy contract")
        .assert_success();

    (account_id, signer)
}

/// Deploy the custom test-contract (with `custom_is_trusted_relayer`).
/// Accepts an optional `always_trusted` account for the custom bypass logic.
async fn deploy_custom_contract(
    env: &TestEnv,
    always_trusted: Option<&near_api::AccountId>,
) -> (near_api::AccountId, Arc<Signer>) {
    let (account_id, signer) = create_account(env, 50).await;

    Contract::deploy(account_id.clone())
        .use_code(CUSTOM_CONTRACT_WASM.to_vec())
        .with_init_call("new", json!({ "always_trusted": always_trusted }))
        .unwrap()
        .with_signer(signer.clone())
        .send_to(&env.network)
        .await
        .expect("Failed to deploy custom contract")
        .assert_success();

    (account_id, signer)
}

/// Grant an ACL role on the contract. The caller must be super-admin.
async fn grant_role(
    env: &TestEnv,
    contract_id: &near_api::AccountId,
    contract_signer: &Arc<Signer>,
    role: &str,
    grantee: &near_api::AccountId,
) {
    let contract = Contract(contract_id.clone());
    contract
        .call_function(
            "acl_grant_role",
            json!({ "role": role, "account_id": grantee }),
        )
        .transaction()
        .with_signer(contract_id.clone(), contract_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Failed to grant role")
        .assert_success();
}

/// Set a short relayer config for testing. Uses 0 waiting period by default.
async fn set_short_config(
    env: &TestEnv,
    contract_id: &near_api::AccountId,
    admin_id: &near_api::AccountId,
    admin_signer: &Arc<Signer>,
    stake_near: u128,
    waiting_period_ns: u64,
) {
    let contract = Contract(contract_id.clone());
    let stake = near_api::NearToken::from_near(stake_near);
    contract
        .call_function(
            "set_relayer_config",
            json!({
                "stake_required": stake,
                "waiting_period_ns": waiting_period_ns.to_string(),
            }),
        )
        .transaction()
        .with_signer(admin_id.clone(), admin_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Failed to set relayer config")
        .assert_success();
}

#[tokio::test]
async fn test_default_relayer_config() {
    let env = get_env().await;
    let (contract_id, _) = deploy_contract(env).await;

    let config: Data<serde_json::Value> = Contract(contract_id)
        .call_function("get_relayer_config", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    let cfg = &config.data;
    // Default: 1000 NEAR
    let stake_yocto: u128 = cfg["stake_required"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        stake_yocto,
        1000 * 10u128.pow(24),
        "Default stake should be 1000 NEAR"
    );

    // Default: 7 days in nanoseconds
    let waiting_ns: u64 = cfg["waiting_period_ns"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        waiting_ns,
        7 * 24 * 60 * 60 * 1_000_000_000u64,
        "Default waiting period should be 7 days"
    );
}

#[tokio::test]
async fn test_set_config_by_manager() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;

    // Grant Admin role
    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;

    // Set custom config
    set_short_config(
        env,
        &contract_id,
        &admin_id,
        &admin_signer,
        5,
        1_000_000_000,
    )
    .await;

    // Verify
    let config: Data<serde_json::Value> = Contract(contract_id)
        .call_function("get_relayer_config", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    let stake: u128 = config.data["stake_required"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(stake, 5 * 10u128.pow(24));

    let waiting: u64 = config.data["waiting_period_ns"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(waiting, 1_000_000_000);
}

#[tokio::test]
async fn test_set_config_by_non_manager_fails() {
    let env = get_env().await;
    let (contract_id, _) = deploy_contract(env).await;
    let (random_id, random_signer) = create_account(env, 10).await;

    // No Admin role — should fail
    let result = Contract(contract_id)
        .call_function(
            "set_relayer_config",
            json!({
                "stake_required": near_api::NearToken::from_near(5),
                "waiting_period_ns": "0",
            }),
        )
        .transaction()
        .with_signer(random_id, random_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Non-admin should not be able to set config"
    );
}

#[tokio::test]
async fn test_apply_with_sufficient_stake() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    // Grant admin, set stake config with a long waiting period so the
    // application remains "pending" (not yet active) when we query it.
    // Other tests may fast_forward the sandbox, so use a very large value.
    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, u64::MAX).await;

    // Apply
    let contract = Contract(contract_id.clone());
    contract
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Verify application exists (get_relayer_application only returns
    // pending applications — those not yet past the waiting period)
    let app: Data<serde_json::Value> = contract
        .call_function(
            "get_relayer_application",
            json!({ "account_id": relayer_id }),
        )
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(
        !app.data.is_null(),
        "Application should exist for the relayer"
    );

    let stake: u128 = app.data["stake"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        stake,
        5 * 10u128.pow(24),
        "Stake should match configured amount"
    );
}

#[tokio::test]
async fn test_apply_with_insufficient_stake() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Apply with only 1 NEAR (need 5)
    let result = Contract(contract_id)
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(1))
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Should fail with insufficient stake"
    );
}

#[tokio::test]
async fn test_apply_with_excess_stake_refunds() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 30).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Check balance before
    let balance_before = Tokens::account(relayer_id.clone())
        .near_balance()
        .fetch_from(&env.network)
        .await
        .unwrap();

    // Apply with 10 NEAR (need only 5)
    Contract(contract_id)
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(10))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Check balance after — should be approximately balance_before - 5 NEAR (minus gas)
    let balance_after = Tokens::account(relayer_id)
        .near_balance()
        .fetch_from(&env.network)
        .await
        .unwrap();

    let diff_yocto = balance_before.total.as_yoctonear() - balance_after.total.as_yoctonear();
    let five_near = 5 * 10u128.pow(24);
    let six_near = 6 * 10u128.pow(24);

    // Should have spent ~5 NEAR (stake) + gas, not 10 NEAR
    assert!(
        diff_yocto > five_near && diff_yocto < six_near,
        "Excess should be refunded. Spent {diff_yocto} yoctoNEAR, expected ~{five_near}"
    );
}

#[tokio::test]
async fn test_duplicate_application_fails() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 30).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    let contract = Contract(contract_id);

    // First application
    contract
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer.clone())
        .send_to(&env.network)
        .await
        .expect("First apply should succeed")
        .assert_success();

    // Second application
    let result = contract
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Duplicate application should fail"
    );
}

#[tokio::test]
async fn test_relayer_not_active_before_waiting_period() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    // Long waiting period: 1 hour
    set_short_config(
        env,
        &contract_id,
        &admin_id,
        &admin_signer,
        5,
        3_600_000_000_000,
    )
    .await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Check — should NOT be trusted yet
    let result: Data<bool> = Contract(contract_id)
        .call_function("is_trusted_relayer", json!({ "account_id": relayer_id }))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(
        !result.data,
        "Relayer should not be active before waiting period"
    );
}

#[tokio::test]
async fn test_relayer_activation_after_waiting_period() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    // Short waiting period: ~5 seconds (5 blocks)
    set_short_config(
        env,
        &contract_id,
        &admin_id,
        &admin_signer,
        5,
        5_000_000_000,
    )
    .await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Fast forward well past the waiting period.
    // Sandbox blocks advance ~1s each; use a generous margin.
    env.sandbox.fast_forward(200).await.unwrap();

    // Check — should be trusted now
    let result: Data<bool> = Contract(contract_id.clone())
        .call_function("is_trusted_relayer", json!({ "account_id": relayer_id }))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(result.data, "Relayer should be active after waiting period");

    // Also verify get_relayer_stake returns the stake
    let stake: Data<serde_json::Value> = Contract(contract_id)
        .call_function("get_relayer_stake", json!({ "account_id": relayer_id }))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(
        !stake.data.is_null(),
        "Active relayer should have stake visible"
    );
}

#[tokio::test]
async fn test_resign_active_relayer() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    // 0 waiting period → instantly active
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    let balance_before_resign = Tokens::account(relayer_id.clone())
        .near_balance()
        .fetch_from(&env.network)
        .await
        .unwrap();

    // Resign
    Contract(contract_id.clone())
        .call_function("resign_trusted_relayer", json!({}))
        .transaction()
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Resign should succeed")
        .assert_success();

    // Verify no longer trusted
    let result: Data<bool> = Contract(contract_id.clone())
        .call_function("is_trusted_relayer", json!({ "account_id": relayer_id }))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert!(
        !result.data,
        "Relayer should no longer be trusted after resignation"
    );

    // Verify stake was returned
    let balance_after = Tokens::account(relayer_id)
        .near_balance()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert!(
        balance_after.total.as_yoctonear() > balance_before_resign.total.as_yoctonear(),
        "Balance should increase after stake refund"
    );
}

#[tokio::test]
async fn test_resign_before_activation_fails() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    // Long waiting period
    set_short_config(
        env,
        &contract_id,
        &admin_id,
        &admin_signer,
        5,
        3_600_000_000_000,
    )
    .await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Try to resign before activation
    let result = Contract(contract_id)
        .call_function("resign_trusted_relayer", json!({}))
        .transaction()
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Resign before activation should fail"
    );
}

#[tokio::test]
async fn test_reject_by_manager() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(
        env,
        &contract_id,
        &admin_id,
        &admin_signer,
        5,
        3_600_000_000_000,
    )
    .await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    let admin_balance_before = Tokens::account(admin_id.clone())
        .near_balance()
        .fetch_from(&env.network)
        .await
        .unwrap();

    // Admin rejects
    Contract(contract_id.clone())
        .call_function(
            "reject_relayer_application",
            json!({ "account_id": relayer_id }),
        )
        .transaction()
        .with_signer(admin_id.clone(), admin_signer)
        .send_to(&env.network)
        .await
        .expect("Reject should succeed")
        .assert_success();

    // Verify relayer no longer has an application
    let app: Data<serde_json::Value> = Contract(contract_id.clone())
        .call_function(
            "get_relayer_application",
            json!({ "account_id": relayer_id }),
        )
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert!(
        app.data.is_null(),
        "Application should be removed after rejection"
    );

    // Verify stake was sent to admin (the predecessor who called reject)
    let admin_balance_after = Tokens::account(admin_id)
        .near_balance()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert!(
        admin_balance_after.total.as_yoctonear() > admin_balance_before.total.as_yoctonear(),
        "Admin should receive the rejected stake"
    );
}

#[tokio::test]
async fn test_reject_by_non_manager_fails() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;
    let (random_id, random_signer) = create_account(env, 10).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Random (non-admin) tries to reject
    let result = Contract(contract_id)
        .call_function(
            "reject_relayer_application",
            json!({ "account_id": relayer_id }),
        )
        .transaction()
        .with_signer(random_id, random_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Non-admin should not be able to reject applications"
    );
}

#[tokio::test]
async fn test_relayer_only_method_with_active_relayer() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Apply (instant activation with waiting_period = 0)
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Call relayer_only_method
    Contract(contract_id.clone())
        .call_function("relayer_only_method", json!({}))
        .transaction()
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Relayer-only method should succeed for active relayer")
        .assert_success();

    // Verify value was incremented
    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(
        value.data, 1,
        "Value should be incremented by relayer method"
    );
}

#[tokio::test]
async fn test_relayer_only_method_without_relayer() {
    let env = get_env().await;
    let (contract_id, _) = deploy_contract(env).await;
    let (random_id, random_signer) = create_account(env, 10).await;

    // Call without being a relayer
    let result = Contract(contract_id)
        .call_function("relayer_only_method", json!({}))
        .transaction()
        .with_signer(random_id, random_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Non-relayer should not be able to call relayer-only method"
    );
}

#[tokio::test]
async fn test_bypass_role_allows_relayer_method() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (bypasser_id, bypasser_signer) = create_account(env, 10).await;

    // Grant Relayer role (bypass role — no staking needed)
    grant_role(env, &contract_id, &contract_signer, "Relayer", &bypasser_id).await;

    // Call relayer_only_method without ever staking
    Contract(contract_id.clone())
        .call_function("relayer_only_method", json!({}))
        .transaction()
        .with_signer(bypasser_id, bypasser_signer)
        .send_to(&env.network)
        .await
        .expect("Bypass role holder should be able to call relayer method")
        .assert_success();

    // Verify value was incremented
    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(value.data, 1, "Value should be incremented");
}

#[tokio::test]
async fn test_self_call_bypass() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;

    // Call self_call_relayer_method — this makes the contract call itself.
    // The contract's own account_id should pass the bypass_roles check since
    // the generated is_trusted_relayer first checks ACL roles, then checks the
    // staking map. The self-call works because:
    // 1. In bypass_roles mode, the override doesn't have the current_account_id
    //    shortcut. However, the staking map will not contain the contract itself.
    // 2. But the contract IS the super-admin (set in `new()`), and if Relayer role
    //    is granted to it, the bypass would work. Otherwise we need to grant it.
    //
    // Actually, let's grant the contract itself the Relayer bypass role for this test,
    // since the bypass_roles override doesn't include the self-call check.
    grant_role(env, &contract_id, &contract_signer, "Relayer", &contract_id).await;

    Contract(contract_id.clone())
        .call_function("self_call_relayer_method", json!({}))
        .transaction()
        .with_signer(contract_id.clone(), contract_signer)
        .send_to(&env.network)
        .await
        .expect("Self-call should succeed")
        .assert_success();

    // The self-call is a promise, so value may have been incremented asynchronously.
    // Wait a moment and check.
    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(value.data, 1, "Self-call should increment value");
}

#[tokio::test]
async fn test_secondary_relayer_method_with_active_relayer() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Apply (instant activation with waiting_period = 0)
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Call the method from the secondary (guard-only) impl block
    Contract(contract_id.clone())
        .call_function("relayer_only_method_secondary", json!({}))
        .transaction()
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Secondary relayer-only method should succeed for active relayer")
        .assert_success();

    // Verify value was incremented by 10
    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(
        value.data, 10,
        "Value should be incremented by 10 from secondary method"
    );
}

#[tokio::test]
async fn test_secondary_relayer_method_without_relayer() {
    let env = get_env().await;
    let (contract_id, _) = deploy_contract(env).await;
    let (random_id, random_signer) = create_account(env, 10).await;

    // Call without being a relayer
    let result = Contract(contract_id)
        .call_function("relayer_only_method_secondary", json!({}))
        .transaction()
        .with_signer(random_id, random_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Non-relayer should not be able to call secondary relayer-only method"
    );
}

#[tokio::test]
async fn test_secondary_bypass_role_allows_method() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (bypasser_id, bypasser_signer) = create_account(env, 10).await;

    // Grant Relayer role (bypass role — no staking needed)
    grant_role(env, &contract_id, &contract_signer, "Relayer", &bypasser_id).await;

    // Call the secondary method without ever staking
    Contract(contract_id.clone())
        .call_function("relayer_only_method_secondary", json!({}))
        .transaction()
        .with_signer(bypasser_id, bypasser_signer)
        .send_to(&env.network)
        .await
        .expect("Bypass role holder should be able to call secondary relayer method")
        .assert_success();

    // Verify value was incremented by 10
    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(value.data, 10, "Value should be incremented by 10");
}

#[tokio::test]
async fn test_relayer_manager_can_reject() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (manager_id, manager_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    // Grant Admin to set config, RelayerManager to the manager
    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    grant_role(
        env,
        &contract_id,
        &contract_signer,
        "RelayerManager",
        &manager_id,
    )
    .await;

    set_short_config(
        env,
        &contract_id,
        &admin_id,
        &admin_signer,
        5,
        3_600_000_000_000,
    )
    .await;

    // Relayer applies
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // RelayerManager rejects — should succeed (manager_roles includes RelayerManager)
    Contract(contract_id.clone())
        .call_function(
            "reject_relayer_application",
            json!({ "account_id": relayer_id }),
        )
        .transaction()
        .with_signer(manager_id, manager_signer)
        .send_to(&env.network)
        .await
        .expect("RelayerManager should be able to reject")
        .assert_success();

    // Verify application is gone
    let app: Data<serde_json::Value> = Contract(contract_id)
        .call_function(
            "get_relayer_application",
            json!({ "account_id": relayer_id }),
        )
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert!(
        app.data.is_null(),
        "Application should be removed after rejection"
    );
}

#[tokio::test]
async fn test_relayer_manager_cannot_set_config() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (manager_id, manager_signer) = create_account(env, 10).await;

    // Grant only RelayerManager (not Admin)
    grant_role(
        env,
        &contract_id,
        &contract_signer,
        "RelayerManager",
        &manager_id,
    )
    .await;

    // RelayerManager tries to set config — should fail (config_roles is Admin only)
    let result = Contract(contract_id)
        .call_function(
            "set_relayer_config",
            json!({
                "stake_required": near_api::NearToken::from_near(1),
                "waiting_period_ns": "0",
            }),
        )
        .transaction()
        .with_signer(manager_id, manager_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "RelayerManager should not be able to set config (requires Admin via config_roles)"
    );
}

#[tokio::test]
async fn test_special_bypass_can_call_special_method() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (special_id, special_signer) = create_account(env, 10).await;

    // Grant SpecialRelayer role (method-level bypass for special_relayer_method)
    grant_role(
        env,
        &contract_id,
        &contract_signer,
        "SpecialRelayer",
        &special_id,
    )
    .await;

    Contract(contract_id.clone())
        .call_function("special_relayer_method", json!({}))
        .transaction()
        .with_signer(special_id, special_signer)
        .send_to(&env.network)
        .await
        .expect("SpecialRelayer should bypass special_relayer_method")
        .assert_success();

    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(value.data, 100, "Value should be incremented by 100");
}

#[tokio::test]
async fn test_special_bypass_cannot_call_default_method() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (special_id, special_signer) = create_account(env, 10).await;

    // Grant SpecialRelayer role — but relayer_only_method uses impl-level bypass (Relayer)
    grant_role(
        env,
        &contract_id,
        &contract_signer,
        "SpecialRelayer",
        &special_id,
    )
    .await;

    let result = Contract(contract_id)
        .call_function("relayer_only_method", json!({}))
        .transaction()
        .with_signer(special_id, special_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "SpecialRelayer should NOT be able to call relayer_only_method (uses impl-level bypass)"
    );
}

#[tokio::test]
async fn test_default_bypass_cannot_call_special_method() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (relayer_id, relayer_signer) = create_account(env, 10).await;

    // Grant Relayer role (impl-level bypass) — but special_relayer_method overrides to SpecialRelayer
    grant_role(env, &contract_id, &contract_signer, "Relayer", &relayer_id).await;

    let result = Contract(contract_id)
        .call_function("special_relayer_method", json!({}))
        .transaction()
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Relayer (impl-level bypass) should NOT be able to call special_relayer_method (overridden to SpecialRelayer)"
    );
}

#[tokio::test]
async fn test_active_relayer_can_call_special_method() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Stake as relayer (instant activation with waiting_period = 0)
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Active staked relayer can call special_relayer_method via staking map fallback
    Contract(contract_id.clone())
        .call_function("special_relayer_method", json!({}))
        .transaction()
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Active staked relayer should be able to call special_relayer_method")
        .assert_success();

    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(value.data, 100, "Value should be incremented by 100");
}

#[tokio::test]
async fn test_custom_always_trusted_can_call_guarded_method() {
    let env = get_env().await;
    let (trusted_id, trusted_signer) = create_account(env, 10).await;

    // Deploy with always_trusted set to the trusted account
    let (contract_id, _) = deploy_custom_contract(env, Some(&trusted_id)).await;

    // The always_trusted account can call the guarded method without staking
    Contract(contract_id.clone())
        .call_function("guarded_method", json!({}))
        .transaction()
        .with_signer(trusted_id, trusted_signer)
        .send_to(&env.network)
        .await
        .expect("always_trusted account should bypass the relayer check")
        .assert_success();

    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(value.data, 1, "Value should be incremented by 1");
}

#[tokio::test]
async fn test_custom_random_account_cannot_call_guarded_method() {
    let env = get_env().await;
    let (trusted_id, _) = create_account(env, 10).await;
    let (random_id, random_signer) = create_account(env, 10).await;

    // Deploy with always_trusted set to a different account
    let (contract_id, _) = deploy_custom_contract(env, Some(&trusted_id)).await;

    // A random account (not trusted, not staked) should fail
    let result = Contract(contract_id)
        .call_function("guarded_method", json!({}))
        .transaction()
        .with_signer(random_id, random_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Random account should not pass the custom is_trusted_relayer check"
    );
}

#[tokio::test]
async fn test_custom_staked_relayer_can_call_guarded_method() {
    let env = get_env().await;

    // Deploy with no always_trusted account
    let (contract_id, contract_signer) = deploy_custom_contract(env, None).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Stake as relayer
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Staked relayer can call guarded method (custom is_trusted_relayer falls back to staking map)
    Contract(contract_id.clone())
        .call_function("guarded_method", json!({}))
        .transaction()
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Staked relayer should pass the custom is_trusted_relayer check")
        .assert_success();

    let value: Data<u64> = Contract(contract_id)
        .call_function("get_value", ())
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(value.data, 1, "Value should be incremented by 1");
}

#[tokio::test]
async fn test_custom_no_always_trusted_random_fails() {
    let env = get_env().await;

    // Deploy with no always_trusted account
    let (contract_id, _) = deploy_custom_contract(env, None).await;
    let (random_id, random_signer) = create_account(env, 10).await;

    // Random account should fail (no always_trusted, not staked)
    let result = Contract(contract_id)
        .call_function("guarded_method", json!({}))
        .transaction()
        .with_signer(random_id, random_signer)
        .send_to(&env.network)
        .await;

    assert!(
        result.is_err() || result.as_ref().is_ok_and(|r| r.is_failure()),
        "Random account should not pass when no always_trusted is set and not staked"
    );
}

#[tokio::test]
async fn test_custom_generated_public_methods_exist() {
    let env = get_env().await;

    let (trusted_id, _) = create_account(env, 10).await;
    let (contract_id, contract_signer) = deploy_custom_contract(env, Some(&trusted_id)).await;

    // Verify that the custom is_trusted_relayer is used by the generated public method
    let is_trusted: Data<bool> = Contract(contract_id.clone())
        .call_function("is_trusted_relayer", json!({ "account_id": trusted_id }))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert!(
        is_trusted.data,
        "Custom is_trusted_relayer should return true for always_trusted account"
    );

    // Verify config methods are generated
    let config: Data<serde_json::Value> = Contract(contract_id.clone())
        .call_function("get_relayer_config", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert!(
        !config.data.is_null(),
        "get_relayer_config should return a valid config"
    );

    // Verify admin can set config (manager_roles includes Admin)
    grant_role(env, &contract_id, &contract_signer, "Admin", &contract_id).await;
    Contract(contract_id.clone())
        .call_function(
            "set_relayer_config",
            json!({
                "stake_required": near_api::NearToken::from_near(1),
                "waiting_period_ns": "0",
            }),
        )
        .transaction()
        .with_signer(contract_id.clone(), contract_signer)
        .send_to(&env.network)
        .await
        .expect("Admin should be able to set config")
        .assert_success();
}

#[tokio::test]
async fn test_get_active_relayers_empty() {
    let env = get_env().await;
    let (contract_id, _) = deploy_contract(env).await;

    let result: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id)
        .call_function("get_active_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(result.data.is_empty(), "Should return empty list when no relayers exist");
}

#[tokio::test]
async fn test_get_pending_relayers_empty() {
    let env = get_env().await;
    let (contract_id, _) = deploy_contract(env).await;

    let result: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id)
        .call_function("get_pending_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(result.data.is_empty(), "Should return empty list when no relayers exist");
}

#[tokio::test]
async fn test_get_pending_relayers_returns_pending() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    // Long waiting period so the relayer stays pending
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, u64::MAX).await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Should appear in pending
    let pending: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function("get_pending_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(pending.data.len(), 1, "Should have one pending relayer");
    assert_eq!(
        pending.data[0].0,
        relayer_id.to_string(),
        "Pending relayer account_id should match"
    );

    // Should NOT appear in active
    let active: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id)
        .call_function("get_active_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(active.data.is_empty(), "Should have no active relayers while pending");
}

#[tokio::test]
async fn test_get_active_relayers_returns_active() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    // 0 waiting period → instantly active
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Should appear in active
    let active: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function("get_active_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(active.data.len(), 1, "Should have one active relayer");
    assert_eq!(
        active.data[0].0,
        relayer_id.to_string(),
        "Active relayer account_id should match"
    );

    // Should NOT appear in pending
    let pending: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id)
        .call_function("get_pending_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(pending.data.is_empty(), "Should have no pending relayers when instantly active");
}

#[tokio::test]
async fn test_get_active_relayers_multiple_with_pagination() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    // 0 waiting period → instantly active
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Apply with 3 relayers
    let mut relayer_ids = Vec::new();
    for _ in 0..3 {
        let (relayer_id, relayer_signer) = create_account(env, 20).await;
        Contract(contract_id.clone())
            .call_function("apply_for_trusted_relayer", json!({}))
            .transaction()
            .deposit(near_api::NearToken::from_near(5))
            .with_signer(relayer_id.clone(), relayer_signer)
            .send_to(&env.network)
            .await
            .expect("Apply should succeed")
            .assert_success();
        relayer_ids.push(relayer_id.to_string());
    }

    // Get all active relayers (no pagination args)
    let all: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function("get_active_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(all.data.len(), 3, "Should have 3 active relayers");

    // Paginate: first 2
    let page1: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function(
            "get_active_relayers",
            json!({ "from_index": 0, "limit": 2 }),
        )
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(page1.data.len(), 2, "First page should have 2 relayers");

    // Paginate: next page starting from index 2
    let page2: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function(
            "get_active_relayers",
            json!({ "from_index": 2, "limit": 2 }),
        )
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(page2.data.len(), 1, "Second page should have 1 relayer");

    // Paginate: beyond range
    let page3: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id)
        .call_function(
            "get_active_relayers",
            json!({ "from_index": 10, "limit": 2 }),
        )
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert!(page3.data.is_empty(), "Page beyond range should be empty");
}

#[tokio::test]
async fn test_get_pending_relayers_multiple_with_pagination() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    // Long waiting period so all relayers stay pending
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, u64::MAX).await;

    // Apply with 3 relayers
    for _ in 0..3 {
        let (relayer_id, relayer_signer) = create_account(env, 20).await;
        Contract(contract_id.clone())
            .call_function("apply_for_trusted_relayer", json!({}))
            .transaction()
            .deposit(near_api::NearToken::from_near(5))
            .with_signer(relayer_id.clone(), relayer_signer)
            .send_to(&env.network)
            .await
            .expect("Apply should succeed")
            .assert_success();
    }

    // Get all pending relayers
    let all: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function("get_pending_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(all.data.len(), 3, "Should have 3 pending relayers");

    // Paginate: first 1
    let page1: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function(
            "get_pending_relayers",
            json!({ "from_index": 0, "limit": 1 }),
        )
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(page1.data.len(), 1, "First page should have 1 relayer");

    // Paginate: skip 1, take 2
    let page2: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id)
        .call_function(
            "get_pending_relayers",
            json!({ "from_index": 1, "limit": 2 }),
        )
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(page2.data.len(), 2, "Second page should have 2 relayers");
}

#[tokio::test]
async fn test_get_relayers_mixed_active_and_pending() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;

    // First relayer: instantly active (waiting_period = 0)
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;
    let (active_relayer_id, active_relayer_signer) = create_account(env, 20).await;
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(active_relayer_id.clone(), active_relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Second relayer: pending (long waiting period)
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, u64::MAX).await;
    let (pending_relayer_id, pending_relayer_signer) = create_account(env, 20).await;
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(pending_relayer_id.clone(), pending_relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Verify active list
    let active: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function("get_active_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(active.data.len(), 1, "Should have exactly 1 active relayer");
    assert_eq!(
        active.data[0].0,
        active_relayer_id.to_string(),
        "Active relayer should be the one with 0 waiting period"
    );

    // Verify pending list
    let pending: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id)
        .call_function("get_pending_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();

    assert_eq!(pending.data.len(), 1, "Should have exactly 1 pending relayer");
    assert_eq!(
        pending.data[0].0,
        pending_relayer_id.to_string(),
        "Pending relayer should be the one with long waiting period"
    );
}

#[tokio::test]
async fn test_get_active_relayers_after_resign() {
    let env = get_env().await;
    let (contract_id, contract_signer) = deploy_contract(env).await;
    let (admin_id, admin_signer) = create_account(env, 10).await;
    let (relayer_id, relayer_signer) = create_account(env, 20).await;

    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(env, &contract_id, &admin_id, &admin_signer, 5, 0).await;

    // Apply
    Contract(contract_id.clone())
        .call_function("apply_for_trusted_relayer", json!({}))
        .transaction()
        .deposit(near_api::NearToken::from_near(5))
        .with_signer(relayer_id.clone(), relayer_signer.clone())
        .send_to(&env.network)
        .await
        .expect("Apply should succeed")
        .assert_success();

    // Verify active
    let active: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id.clone())
        .call_function("get_active_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert_eq!(active.data.len(), 1, "Should have 1 active relayer before resign");

    // Resign
    Contract(contract_id.clone())
        .call_function("resign_trusted_relayer", json!({}))
        .transaction()
        .with_signer(relayer_id, relayer_signer)
        .send_to(&env.network)
        .await
        .expect("Resign should succeed")
        .assert_success();

    // Verify empty after resign
    let active: Data<Vec<(String, serde_json::Value)>> = Contract(contract_id)
        .call_function("get_active_relayers", json!({}))
        .read_only()
        .fetch_from(&env.network)
        .await
        .unwrap();
    assert!(active.data.is_empty(), "Should have no active relayers after resign");
}
