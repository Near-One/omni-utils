use near_plugins::{AccessControlRole, AccessControllable, access_control};
use near_sdk::{AccountId, PanicOnDefault, env, near};
use omni_utils::macros::trusted_relayer;
use omni_utils::trusted_relayer::TrustedRelayer;

#[derive(AccessControlRole, Copy, Clone)]
#[near(serializers=[json])]
pub enum Role {
    Admin,
}

#[access_control(role_type(Role))]
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct CustomContract {
    value: u64,
    /// An account that is always considered a trusted relayer.
    always_trusted: Option<AccountId>,
}

/// Custom implementation of `TrustedRelayer` — the macro won't generate this
/// because of the `custom_is_trusted_relayer` flag.
impl TrustedRelayer for CustomContract {
    fn is_trusted_relayer(&self, account_id: &AccountId) -> bool {
        // Custom logic: check the always_trusted allowlist first
        if let Some(trusted) = &self.always_trusted {
            if account_id == trusted {
                return true;
            }
        }

        // Self-call bypass
        if *account_id == env::current_account_id() {
            return true;
        }

        // Fall back to the standard staking map check
        omni_utils::trusted_relayer::tr_relayers_map()
            .get(account_id)
            .is_some_and(|state| env::block_timestamp() >= state.activate_at.0)
    }
}

#[trusted_relayer(
    manager_roles(Role::Admin),
    config_roles(Role::Admin),
    custom_is_trusted_relayer
)]
#[near]
impl CustomContract {
    #[init]
    pub fn new(always_trusted: Option<AccountId>) -> Self {
        let mut contract = Self {
            value: 0,
            always_trusted,
        };
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
    pub fn guarded_method(&mut self) -> u64 {
        self.value += 1;
        self.value
    }
}
