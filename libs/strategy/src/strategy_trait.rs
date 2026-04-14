//! Strategy trait definitions.

pub trait Strategy: Send + Sync + Clone {
    fn name(&self) -> &str;
    fn clone_box(&self) -> Box<dyn Strategy>;
}

impl Clone for Box<dyn Strategy> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}
