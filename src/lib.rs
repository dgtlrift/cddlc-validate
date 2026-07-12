//! Runtime validation of decoded JSON/CBOR data against a resolved
//! [`cddlc_ir::IrModule`].
//!
//! # Known limitation: tagged prelude types
//!
//! `cddlc-ir` lowers the RFC 8610 Appendix D "tagged" prelude names
//! (`tdate`, `time`, `uri`, `b64url`, `biguint`, `bigint`, `encoded-cbor`,
//! `regexp`, `mime-message`, `cbor-any`, ...) to `Primitive::Any` during
//! lowering — the name itself doesn't survive into the IR, only the fact
//! that *some* value is expected (see `resolve_typeref` in
//! `cddlc-ir/src/lower.rs`). Fields declared with these prelude names
//! therefore validate permissively (any value passes) rather than being
//! checked against their RFC 8949 §6.1 JSON mapping (e.g. `tdate` should be
//! an RFC 3339 string). Explicit tags written directly in a schema (e.g.
//! `mydate = #6.0(tstr)`) are unaffected — `TypeDef::tagged` does survive
//! lowering and is checked normally. Fixing the prelude case would require
//! adding new `Primitive` variants, which are matched exhaustively by all
//! seven codegen backends — out of scope here; tracked as follow-up work.

pub mod value;
pub mod error;
pub mod engine;

pub use error::ValidationError;
pub use value::Value;

use cddlc_ir::IrModule;

/// Validate a decoded [`Value`] against the named type in `module`.
///
/// Pass `type_name = None` to validate against the schema's declared root
/// type ([`IrModule::root`]).
pub fn validate(
    module:    &IrModule,
    type_name: Option<&str>,
    value:     &Value,
) -> Result<(), Vec<ValidationError>> {
    let root = type_name.unwrap_or(module.root.as_str());
    engine::validate_named(module, root, value)
}
