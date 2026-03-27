use near_sdk::borsh::{self, BorshDeserialize};
use near_sdk::json_types::{U64, U128};
use near_sdk::serde_json::json;
use near_sdk::store::IterableMap;
use near_sdk::{AccountId, NearToken, Promise, env, near, require};

const TR_CONFIG_KEY: &[u8] = b"__tr_config";
const TR_RELAYERS_PREFIX: &[u8] = b"__tr_relayers";
const TR_RELAYERS_META_KEY: &[u8] = b"__tr_relayers_meta";

#[derive(Debug, Clone)]
#[near(serializers = [json, borsh])]
pub struct RelayerState {
    pub stake: NearToken,
    pub activate_at: U64,
}

#[derive(Debug, Clone)]
#[near(serializers = [json, borsh])]
pub struct RelayerConfig {
    pub stake_required: NearToken,
    pub waiting_period_ns: U64,
}

#[derive(Debug, Clone)]
#[near(serializers = [json])]
pub enum TrustedRelayerEvent {
    RelayerApplyEvent {
        account_id: AccountId,
        stake: NearToken,
        activate_at: U64,
    },
    RelayerResignEvent {
        account_id: AccountId,
        stake: NearToken,
    },
    RelayerRejectEvent {
        account_id: AccountId,
        stake: NearToken,
    },
}

impl TrustedRelayerEvent {
    pub fn emit(&self) {
        env::log_str(&json!(self).to_string());
    }
}

impl Default for RelayerConfig {
    fn default() -> Self {
        Self {
            stake_required: NearToken::from_near(1000),
            waiting_period_ns: U64(7 * 24 * 60 * 60 * 1_000_000_000),
        }
    }
}

pub fn tr_load_config() -> RelayerConfig {
    env::storage_read(TR_CONFIG_KEY)
        .map(|bytes| {
            BorshDeserialize::try_from_slice(&bytes)
                .unwrap_or_else(|_| env::panic_str("Failed to deserialize RelayerConfig"))
        })
        .unwrap_or_default()
}

pub fn tr_save_config(config: &RelayerConfig) {
    env::storage_write(
        TR_CONFIG_KEY,
        &borsh::to_vec(config)
            .unwrap_or_else(|_| env::panic_str("Failed to serialize RelayerConfig")),
    );
}

/// Load the relayers map, restoring iteration metadata from storage.
/// On first use (no metadata yet), creates a fresh empty map.
pub fn tr_load_relayers() -> IterableMap<AccountId, RelayerState> {
    match env::storage_read(TR_RELAYERS_META_KEY) {
        Some(bytes) => BorshDeserialize::try_from_slice(&bytes)
            .unwrap_or_else(|_| env::panic_str("Failed to deserialize relayers map metadata")),
        None => IterableMap::new(TR_RELAYERS_PREFIX),
    }
}

/// Flush pending writes and persist the relayers map metadata (length + prefixes)
/// so that future calls can restore iteration state.
pub fn tr_save_relayers(map: IterableMap<AccountId, RelayerState>) {
    env::storage_write(
        TR_RELAYERS_META_KEY,
        &borsh::to_vec(&map)
            .unwrap_or_else(|_| env::panic_str("Failed to serialize relayers map metadata")),
    );
}

/// Trusted relayer staking support for NEAR contracts.
///
/// Override `is_trusted_relayer` to add custom bypass logic (e.g. ACL roles).
/// The `_tr_*` methods are internal — the `#[trusted_relayer]` proc macro
/// generates the public NEAR-callable wrappers that delegate to these.
pub trait TrustedRelayer {
    /// Default: self-call bypass + staking map check.
    fn is_trusted_relayer(&self, account_id: &AccountId) -> bool {
        if *account_id == env::current_account_id() {
            return true;
        }

        tr_load_relayers()
            .get(account_id)
            .is_some_and(|state| env::block_timestamp() >= state.activate_at.0)
    }

    fn _tr_apply(&mut self) {
        let account_id = env::predecessor_account_id();
        let mut relayers = tr_load_relayers();

        require!(
            relayers.get(&account_id).is_none(),
            "Relayer application already exists"
        );

        let config = tr_load_config();
        let attached = env::attached_deposit();
        require!(
            attached >= config.stake_required,
            "Insufficient stake for relayer application"
        );

        let activate_at = U64(env::block_timestamp().saturating_add(config.waiting_period_ns.0));
        let excess = NearToken::from_yoctonear(
            attached
                .as_yoctonear()
                .saturating_sub(config.stake_required.as_yoctonear()),
        );

        relayers.insert(
            account_id.clone(),
            RelayerState {
                stake: config.stake_required,
                activate_at,
            },
        );

        tr_save_relayers(relayers);

        TrustedRelayerEvent::RelayerApplyEvent {
            account_id: account_id.clone(),
            stake: config.stake_required,
            activate_at,
        }
        .emit();

        if excess.as_yoctonear() > 0 {
            Promise::new(account_id).transfer(excess).detach();
        }
    }

    fn _tr_resign(&mut self) -> Promise {
        let account_id = env::predecessor_account_id();
        let mut relayers = tr_load_relayers();

        let state = relayers
            .remove(&account_id)
            .unwrap_or_else(|| env::panic_str("Relayer not found"));

        require!(
            env::block_timestamp() >= state.activate_at.0,
            "Relayer is not active yet"
        );

        tr_save_relayers(relayers);

        TrustedRelayerEvent::RelayerResignEvent {
            account_id: account_id.clone(),
            stake: state.stake,
        }
        .emit();

        Promise::new(account_id).transfer(state.stake)
    }

    fn _tr_reject(&mut self, account_id: AccountId) -> Promise {
        let mut relayers = tr_load_relayers();

        let state = relayers
            .remove(&account_id)
            .unwrap_or_else(|| env::panic_str("Relayer application not found"));

        tr_save_relayers(relayers);

        TrustedRelayerEvent::RelayerRejectEvent {
            account_id: account_id.clone(),
            stake: state.stake,
        }
        .emit();

        Promise::new(env::predecessor_account_id()).transfer(state.stake)
    }

    fn _tr_set_config(&mut self, stake_required: NearToken, waiting_period_ns: U64) {
        tr_save_config(&RelayerConfig {
            stake_required,
            waiting_period_ns,
        });
    }

    fn _tr_get_application(&self, account_id: &AccountId) -> Option<RelayerState> {
        tr_load_relayers()
            .get(account_id)
            .filter(|state| env::block_timestamp() < state.activate_at.0)
            .cloned()
    }

    fn _tr_get_stake(&self, account_id: &AccountId) -> Option<U128> {
        tr_load_relayers()
            .get(account_id)
            .filter(|state| env::block_timestamp() >= state.activate_at.0)
            .map(|state| U128(state.stake.as_yoctonear()))
    }

    fn _tr_get_config(&self) -> RelayerConfig {
        tr_load_config()
    }

    fn _tr_get_active_relayers(
        &self,
        from_index: Option<u32>,
        limit: Option<u32>,
    ) -> Vec<(AccountId, RelayerState)> {
        let relayers = tr_load_relayers();
        let now = env::block_timestamp();
        relayers
            .iter()
            .filter(|(_, state)| now >= state.activate_at.0)
            .skip(from_index.unwrap_or(0) as usize)
            .take(limit.unwrap_or(100) as usize)
            .map(|(id, state)| (id.clone(), state.clone()))
            .collect()
    }

    fn _tr_get_pending_relayers(
        &self,
        from_index: Option<u32>,
        limit: Option<u32>,
    ) -> Vec<(AccountId, RelayerState)> {
        let relayers = tr_load_relayers();
        let now = env::block_timestamp();
        relayers
            .iter()
            .filter(|(_, state)| now < state.activate_at.0)
            .skip(from_index.unwrap_or(0) as usize)
            .take(limit.unwrap_or(100) as usize)
            .map(|(id, state)| (id.clone(), state.clone()))
            .collect()
    }
}
