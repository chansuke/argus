//! One module per external tool. Each owns both directions of its format: how to invoke
//! the tool and how to read what it wrote. Private to the crate: views see only
//! `Observation`s, the host sees only `CommandSpec`s.

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod eas;
pub(crate) mod lean;
pub(crate) mod llm;
pub(crate) mod tlc;
