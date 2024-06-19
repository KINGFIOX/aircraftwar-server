pub trait Run<T, E: std::error::Error> {
    fn run(&self) -> Result<T, E>;
}
