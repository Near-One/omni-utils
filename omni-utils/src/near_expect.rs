pub trait NearExpect<T> {
    fn near_expect(self, msg: impl AsRef<str>) -> T;
}

impl<T> NearExpect<T> for Option<T> {
    fn near_expect(self, msg: impl AsRef<str>) -> T {
        self.unwrap_or_else(|| near_sdk::env::panic_str(msg.as_ref()))
    }
}

impl<T, E> NearExpect<T> for Result<T, E> {
    fn near_expect(self, msg: impl AsRef<str>) -> T {
        self.unwrap_or_else(|_| near_sdk::env::panic_str(msg.as_ref()))
    }
}
