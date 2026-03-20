//! Constant value generator — returns the same value every tick.

use super::ValueGenerator;

pub struct Constant {
    value: f64,
}

impl Constant {
    pub fn new(value: f64) -> Self {
        Self { value }
    }
}

impl ValueGenerator for Constant {
    fn value(&self, _tick: u64) -> f64 {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_constant_value() {
        let gen = Constant::new(42.0);
        assert_eq!(gen.value(0), 42.0);
        assert_eq!(gen.value(1), 42.0);
        assert_eq!(gen.value(1_000_000), 42.0);
    }
}
