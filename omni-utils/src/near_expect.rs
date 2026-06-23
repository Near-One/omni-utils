pub trait NearExpect<T> {
    fn near_expect(self, msg: impl AsRef<str>) -> T;
}

impl<T> NearExpect<T> for Option<T> {
    fn near_expect(self, msg: impl AsRef<str>) -> T {
        self.unwrap_or_else(|| near_sdk::env::panic_str(msg.as_ref()))
    }
}

impl<T, E: core::fmt::Debug> NearExpect<T> for Result<T, E> {
    fn near_expect(self, msg: impl AsRef<str>) -> T {
        self.unwrap_or_else(|err| near_sdk::env::panic_str(&format!("{}: {:?}", msg.as_ref(), err)))
    }
}
