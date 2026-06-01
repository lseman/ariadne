//! Sample Rust fixture for integration tests.

pub struct Calculator {
    value: f64,
}

impl Calculator {
    pub fn new(value: f64) -> Self {
        Self { value }
    }

    pub fn add(&mut self, rhs: f64) {
        self.value += rhs;
    }

    pub fn multiply(&mut self, rhs: f64) {
        self.value *= rhs;
    }

    pub fn get(&self) -> f64 {
        self.value
    }

    pub fn reset(&mut self) {
        self.value = 0.0;
    }
}

/// Calculate the square of a number.
pub fn square(x: f64) -> f64 {
    x * x
}

/// Calculate the square root using Newton's method.
pub fn sqrt(x: f64) -> f64 {
    if x < 0.0 {
        return f64::NAN;
    }
    let mut guess = x / 2.0;
    for _ in 0..100 {
        let next = (guess + x / guess) / 2.0;
        if (next - guess).abs() < 1e-10 {
            break;
        }
        guess = next;
    }
    guess
}
