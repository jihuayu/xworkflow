//! Template rendering engine.
//!
//! Provides two template syntaxes:
//! - **Answer-node syntax** (`{{#node.var#}}`) — resolved via [`render_template`](engine::render_template).
//! - **Jinja2 syntax** — uses `minijinja` (behind the `builtin-template-jinja` feature).
//!
//! Both support sync and async rendering with optional strict mode.

pub mod engine;

#[cfg(feature = "builtin-template-jinja")]
pub mod jinja;

pub use engine::*;
