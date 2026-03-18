use near_plugins::{AccessControlRole, AccessControllable, access_control};
use near_sdk::{PanicOnDefault, Promise, env, near};
use omni_utils::macros::trusted_relayer;

#[derive(AccessControlRole, Copy, Clone)]
#[near(serializers=[json])]
pub enum Role {
    Admin,
    RelayerManager,
    Relayer,
}

#[access_control(role_type(Role))]
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct TestContract {
    value: u64,
}

#[trusted_relayer(
    manager_roles(Role::Admin, Role::RelayerManager),
    config_roles(Role::Admin),
    bypass_roles(Role::Relayer),
)]
#[near]
impl TestContract {
    #[init]
    pub fn new() -> Self {
        let mut contract = Self { value: 0 };
        near_sdk::require!(
            contract.acl_init_super_admin(env::current_account_id()),
            "Failed to initialize super admin",
        );
        contract
    }

    pub fn get_value(&self) -> u64 {
        self.value
    }

    #[trusted_relayer]
    pub fn relayer_only_method(&mut self) -> u64 {
        self.value += 1;
        self.value
    }

    /// A helper that performs a self-call to `relayer_only_method`.
    /// This lets us test the self-call bypass (current_account_id is always trusted
    /// in the bypass_roles variant because acl_has_any_role is checked, but the
    /// default trait also bypasses self-calls).
    pub fn self_call_relayer_method(&mut self) -> Promise {
        let contract_id = env::current_account_id();
        Promise::new(contract_id).function_call(
            "relayer_only_method".to_string(),
            b"{}".to_vec(),
            near_sdk::NearToken::from_yoctonear(0),
            near_sdk::Gas::from_tgas(10),
        )
    }
}

/// Second impl block using guard-only mode (`#[trusted_relayer]` without args).
/// This tests the multi-impl-block pattern where only the primary block above
/// carries `manager_roles`/`bypass_roles` and generates the public methods.
#[trusted_relayer]
#[near]
impl TestContract {
    /// Guarded method in a separate impl block — only active relayers
    /// (or bypass-role holders) can call this.
    #[trusted_relayer]
    pub fn relayer_only_method_secondary(&mut self) -> u64 {
        self.value += 10;
        self.value
    }

    /// Unguarded method in the secondary block — anyone can call it.
    pub fn get_value_secondary(&self) -> u64 {
        self.value
    }
}
