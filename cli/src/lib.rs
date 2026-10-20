//! Nothing: `verge` is the binary in `main.rs`. This library exists so that the package's tests
//! can depend on the package itself with its `simulate` feature on, which they need to run the
//! binary against the simulated bench. Cargo ignores such a dependency on a binary-only package.
