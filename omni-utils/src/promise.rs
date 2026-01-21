use near_sdk::{Promise, PromiseIndex, env};
use serde::Serialize;

pub enum PromiseOrPromiseIndexOrValue<T> {
    Promise(Promise),
    PromiseIndex(PromiseIndex),
    Value(T),
}

impl<T> PromiseOrPromiseIndexOrValue<T>
where
    T: Serialize,
{
    #[allow(clippy::wrong_self_convention)]
    pub fn as_return(self) {
        match self {
            PromiseOrPromiseIndexOrValue::Promise(promise) => {
                promise.as_return().detach();
            }
            PromiseOrPromiseIndexOrValue::PromiseIndex(promise_index) => {
                env::promise_return(promise_index);
            }
            PromiseOrPromiseIndexOrValue::Value(value) => {
                env::value_return(serde_json::to_vec(&value).unwrap());
            }
        }
    }
}

impl<T> From<Promise> for PromiseOrPromiseIndexOrValue<T> {
    fn from(promise: Promise) -> Self {
        PromiseOrPromiseIndexOrValue::Promise(promise)
    }
}
