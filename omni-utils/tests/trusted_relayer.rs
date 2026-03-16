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

fn build_wasm(path: &str, target_dir: &str) -> Vec<u8> {
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
        ..Default::default()
    })
    .unwrap_or_else(|err| panic!("building contract from {path}: {err:?}"));

    std::fs::read(&artifact.path).unwrap()
}

static CONTRACT_WASM: LazyLock<Vec<u8>> =
    LazyLock::new(|| build_wasm("../tests/test-contract/Cargo.toml", "test-contract-build"));

struct TestEnv {
    sandbox: Sandbox,
    network: NetworkConfig,
    root_signer: Arc<Signer>,
    counter: AtomicUsize,
}

static ENV: OnceCell<TestEnv> = OnceCell::const_new();

async fn get_env() -> &'static TestEnv {
    ENV.get_or_init(|| async {
        // Force WASM build before starting sandbox
        let _ = &*CONTRACT_WASM;

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

    // Grant admin, set low stake config
    grant_role(env, &contract_id, &contract_signer, "Admin", &admin_id).await;
    set_short_config(
        env,
        &contract_id,
        &admin_id,
        &admin_signer,
        5,
        1_000_000_000,
    )
    .await;

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

    // Verify application exists
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
