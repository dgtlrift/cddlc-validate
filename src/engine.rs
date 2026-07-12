//! The validation walk: match a decoded [`Value`] against a resolved
//! [`cddlc_ir::IrModule`], producing a list of [`ValidationError`]s.

use base64::Engine as _;

use cddlc_ir::{
    CborTag, Constraint, FieldDef, FieldKey, IrModule, LiteralValue, Occurrence,
    Primitive, TypeDef, TypeRef,
};

use crate::error::{PathSeg, ValidationError};
use crate::value::Value;

/// Recursive descent limit — a defensive guard against pathologically deep
/// (or malicious) input, since a validator sits at a trust boundary.
const MAX_DEPTH: usize = 128;

pub fn validate_named(
    module:    &IrModule,
    type_name: &str,
    value:     &Value,
) -> Result<(), Vec<ValidationError>> {
    if type_name.is_empty() {
        return Err(vec![ValidationError::new(
            vec![],
            "no root type: the schema defines no rules, or none was given via --type",
        )]);
    }
    let Some(def) = module.get(type_name) else {
        return Err(vec![ValidationError::new(
            vec![],
            format!("unknown type '{type_name}' (not defined in this schema)"),
        )]);
    };

    let mut ctx = Ctx { module, errors: Vec::new(), depth: 0 };
    let mut path = Vec::new();
    ctx.check_def(def, value, &mut path);
    if ctx.errors.is_empty() { Ok(()) } else { Err(ctx.errors) }
}

struct Ctx<'m> {
    module:  &'m IrModule,
    errors:  Vec<ValidationError>,
    depth:   usize,
}

impl<'m> Ctx<'m> {
    fn err(&mut self, path: &[PathSeg], message: impl Into<String>) {
        self.errors.push(ValidationError::new(path.to_vec(), message.into()));
    }

    /// Run `f` in a fresh, isolated error-collecting context (used to try one
    /// alternative of a type choice / enum variant without polluting the
    /// caller's error list on failure).
    fn try_check(&self, tr: &TypeRef, value: &Value, path: &[PathSeg]) -> Vec<ValidationError> {
        let mut sub = Ctx { module: self.module, errors: Vec::new(), depth: self.depth };
        let mut p = path.to_vec();
        sub.check_typeref(tr, value, &mut p);
        sub.errors
    }

    /// Like [`Self::try_check`], but also applies an enum variant's own
    /// constraints (the literal value that discriminates it from sibling
    /// variants sharing the same underlying primitive — see [`cddlc_ir::EnumVariant`]).
    fn try_check_variant(&self, variant: &cddlc_ir::EnumVariant, value: &Value, path: &[PathSeg]) -> Vec<ValidationError> {
        let mut sub = Ctx { module: self.module, errors: Vec::new(), depth: self.depth };
        let mut p = path.to_vec();
        sub.check_typeref(&variant.ty, value, &mut p);
        sub.check_constraints(&variant.constraints, &variant.ty, value, &mut p);
        sub.errors
    }

    fn guard_depth(&mut self, path: &[PathSeg]) -> bool {
        if self.depth >= MAX_DEPTH {
            self.err(path, format!("exceeded maximum nesting depth ({MAX_DEPTH})"));
            return false;
        }
        self.depth += 1;
        true
    }

    // ── Type definitions ─────────────────────────────────────────────────

    fn check_def(&mut self, def: &TypeDef, value: &Value, path: &mut Vec<PathSeg>) {
        if !self.guard_depth(path) { return; }
        match def {
            TypeDef::Struct(s) => {
                if let Some(payload) = self.unwrap_tag(s.tagged, value, path) {
                    self.check_struct(&s.fields, &s.name, payload, path);
                }
            }
            TypeDef::Enum(e) => {
                if let Some(payload) = self.unwrap_tag(e.tagged, value, path) {
                    self.check_enum(&e.variants, &e.name, payload, path);
                }
            }
            TypeDef::Array(a) => {
                if let Some(payload) = self.unwrap_tag(a.tagged, value, path) {
                    self.check_array(&a.element, &a.occurrence, &a.name, payload, path);
                }
            }
            TypeDef::Map(m) => {
                // CDDL tables ({* K => V}) are not constructed by cddlc-ir's
                // lowering pass today (nor fully handled by every codegen
                // backend) — see cddlc-validate::lib docs. Fail clearly
                // rather than silently mis-validating.
                self.err(path, format!(
                    "'{}' uses a CDDL table ({{* K => V}}), which this validator \
                     does not yet support",
                    m.name
                ));
            }
            TypeDef::Alias(a) => {
                if let Some(payload) = self.unwrap_tag(a.tagged, value, path) {
                    self.check_typeref(&a.ty, payload, path);
                    self.check_constraints(&a.constraints, &a.ty, payload, path);
                }
            }
        }
        self.depth -= 1;
    }

    fn check_struct(&mut self, fields: &[FieldDef], name: &str, value: &Value, path: &mut Vec<PathSeg>) {
        let Value::Map(entries) = value else {
            self.err(path, format!("expected a map for struct '{name}', found {}", kind_name(value)));
            return;
        };

        for field in fields {
            match entries.iter().find(|(k, _)| field_key_matches(&field.key, k)) {
                Some((_, v)) => {
                    path.push(PathSeg::Field(field.name.to_string()));
                    self.check_typeref(&field.ty, v, path);
                    self.check_constraints(&field.constraints, &field.ty, v, path);
                    path.pop();
                }
                None => {
                    if field.occurrence.is_required() {
                        self.err(path, format!("missing required field '{}'", field.name));
                    }
                }
            }
        }

        for (k, _) in entries {
            if !fields.iter().any(|f| field_key_matches(&f.key, k)) {
                self.err(path, format!("unknown field {}", describe_key(k)));
            }
        }
    }

    fn check_enum(&mut self, variants: &[cddlc_ir::EnumVariant], name: &str, value: &Value, path: &mut Vec<PathSeg>) {
        let mut attempts: Vec<(String, Vec<ValidationError>)> = Vec::new();
        for variant in variants {
            let errs = self.try_check_variant(variant, value, path);
            if errs.is_empty() {
                return;
            }
            attempts.push((variant.name.to_string(), errs));
        }
        let mut msg = format!("value matched no variant of '{name}'");
        for (vname, errs) in &attempts {
            if let Some(first) = errs.first() {
                msg.push_str(&format!(" [{vname}: {}]", first.message));
            }
        }
        self.err(path, msg);
    }

    fn check_array(&mut self, element: &TypeRef, occurrence: &Occurrence, name: &str, value: &Value, path: &mut Vec<PathSeg>) {
        let Value::Array(items) = value else {
            self.err(path, format!("expected an array for '{name}', found {}", kind_name(value)));
            return;
        };
        check_cardinality(occurrence, items.len(), name, self, path);
        for (i, item) in items.iter().enumerate() {
            path.push(PathSeg::Index(i));
            self.check_typeref(element, item, path);
            path.pop();
        }
    }

    // ── Type references ──────────────────────────────────────────────────

    fn check_typeref(&mut self, tr: &TypeRef, value: &Value, path: &mut Vec<PathSeg>) {
        match tr {
            TypeRef::Primitive(p) => self.check_primitive(p, value, path),
            TypeRef::Named(name) => match self.module.get(name) {
                Some(def) => self.check_def(def, value, path),
                None => self.err(path, format!("internal error: type '{name}' is referenced but not defined")),
            },
            TypeRef::Choice(variants) => {
                let mut attempts: Vec<Vec<ValidationError>> = Vec::new();
                for v in variants {
                    let errs = self.try_check(v, value, path);
                    if errs.is_empty() {
                        return;
                    }
                    attempts.push(errs);
                }
                let detail = attempts.iter()
                    .filter_map(|e| e.first())
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ");
                self.err(path, format!("value did not match any alternative: {detail}"));
            }
            TypeRef::Tagged(tag, inner) => {
                if let Some(payload) = self.unwrap_tag(Some(*tag), value, path) {
                    self.check_typeref(inner, payload, path);
                }
            }
        }
    }

    fn check_primitive(&mut self, p: &Primitive, value: &Value, path: &mut Vec<PathSeg>) {
        let ok = match (p, value) {
            (Primitive::Any, _) => true,
            (Primitive::Bool, Value::Bool(_)) => true,
            (Primitive::Null, Value::Null) => true,
            // Canonical `Value` has no distinct "undefined" — both JSON and
            // ciborium's data model collapse it into Null. See lib.rs docs.
            (Primitive::Undefined, Value::Null) => true,
            (Primitive::Uint, Value::Uint(_)) => true,
            (Primitive::Int, Value::Uint(_) | Value::Int(_)) => true,
            (Primitive::Float16 | Primitive::Float32 | Primitive::Float64 | Primitive::Float,
             Value::Float(_) | Value::Uint(_) | Value::Int(_)) => true,
            (Primitive::Tstr, Value::Text(_)) => true,
            (Primitive::Bstr, Value::Bytes(_)) => true,
            (Primitive::Bstr, Value::Text(s)) => decode_base64url(s).is_some(),
            _ => false,
        };
        if !ok {
            self.err(path, format!("expected {}, found {}", primitive_name(p), kind_name(value)));
        }
    }

    fn unwrap_tag<'v>(&mut self, tagged: Option<CborTag>, value: &'v Value, path: &mut Vec<PathSeg>) -> Option<&'v Value> {
        let Some(tag) = tagged else { return Some(value) };
        match value {
            Value::Tag(n, inner) if *n == tag.0 => Some(inner.as_ref()),
            Value::Tag(n, _) => {
                self.err(path, format!("expected CBOR tag {}, found tag {n}", tag.0));
                None
            }
            // No native tag in the source (JSON, or an untagged CBOR item) —
            // degrade and validate the payload directly. See lib.rs docs.
            _ => Some(value),
        }
    }

    // ── Resolve a TypeRef down to its ultimate Primitive, when unambiguous ─
    // Used only to disambiguate how `.size` should measure a `Value::Text`
    // (decoded base64url bytes for a `bstr` field vs. UTF-8 length for `tstr`).

    fn base_primitive(&self, tr: &TypeRef, depth: usize) -> Option<Primitive> {
        if depth > MAX_DEPTH {
            return None;
        }
        match tr {
            TypeRef::Primitive(p) => Some(p.clone()),
            TypeRef::Named(name) => match self.module.get(name) {
                Some(TypeDef::Alias(a)) => self.base_primitive(&a.ty, depth + 1),
                _ => None,
            },
            TypeRef::Tagged(_, inner) => self.base_primitive(inner, depth + 1),
            TypeRef::Choice(_) => None,
        }
    }

    // ── Constraints ──────────────────────────────────────────────────────

    fn check_constraints(&mut self, constraints: &[Constraint], base_ty: &TypeRef, value: &Value, path: &mut Vec<PathSeg>) {
        if constraints.is_empty() {
            return;
        }
        let prim = self.base_primitive(base_ty, 0);
        for c in constraints {
            match c {
                Constraint::SizeExact(n) => self.check_size(&prim, value, Some(*n), Some(*n), path),
                Constraint::SizeRange { min, max } => self.check_size(&prim, value, *min, *max, path),
                Constraint::ValueRangeInt { min, max, inclusive } =>
                    self.check_range(value, min.map(|n| n as i128), max.map(|n| n as i128), *inclusive, path),
                Constraint::ValueRangeUint { min, max, inclusive } =>
                    self.check_range(value, min.map(|n| n as i128), max.map(|n| n as i128), *inclusive, path),
                Constraint::ValueRangeF64 { min, max, inclusive } =>
                    self.check_range_f64(value, *min, *max, *inclusive, path),
                Constraint::Eq(lit) => {
                    if !literal_eq(lit, value) {
                        self.err(path, format!("expected value equal to {}, found {}", describe_literal(lit), kind_name(value)));
                    }
                }
                Constraint::Ne(lit) => {
                    if literal_eq(lit, value) {
                        self.err(path, format!("value must not equal {}", describe_literal(lit)));
                    }
                }
                Constraint::Regexp { pattern, .. } => self.check_regexp(value, pattern, path),
                Constraint::CborEmbedded(inner) => self.check_cbor_embedded(value, inner, false, path),
                Constraint::CborSeq(inner) => self.check_cbor_embedded(value, inner, true, path),
                // Informational only — does not relax presence/type checks.
                Constraint::Default(_) => {}
            }
        }
    }

    fn check_size(&mut self, prim: &Option<Primitive>, value: &Value, min: Option<usize>, max: Option<usize>, path: &mut Vec<PathSeg>) {
        // `.size` on an integer bounds its byte-width representation, not a
        // container length (RFC 8610 §3.8.1), matching the semantics
        // `backend-c`'s codegen already assumes for numeric `.size`.
        if matches!(prim, Some(Primitive::Uint) | Some(Primitive::Int)) {
            let n: Option<i128> = match value {
                Value::Uint(u) => Some(*u as i128),
                Value::Int(i)  => Some(*i as i128),
                _ => None,
            };
            let Some(n) = n else {
                self.err(path, format!("expected an integer, found {}", kind_name(value)));
                return;
            };
            if let Some(bytes) = max {
                let bits = (bytes as u32) * 8;
                let (lo, hi) = if matches!(prim, Some(Primitive::Uint)) {
                    (0i128, (1i128 << bits) - 1)
                } else {
                    (-(1i128 << (bits.saturating_sub(1))), (1i128 << bits.saturating_sub(1)) - 1)
                };
                if n < lo || n > hi {
                    self.err(path, format!("value {n} does not fit in {bytes} byte(s)"));
                }
            }
            return;
        }

        let len = match value {
            Value::Bytes(b) => b.len(),
            Value::Text(s) => match prim {
                Some(Primitive::Bstr) => decode_base64url(s).map(|b| b.len()).unwrap_or(s.len()),
                _ => s.len(),
            },
            Value::Array(a) => a.len(),
            _ => {
                self.err(path, format!(".size constraint does not apply to {}", kind_name(value)));
                return;
            }
        };
        if let Some(min) = min {
            if len < min {
                self.err(path, format!(".size: expected at least {min}, found {len}"));
                return;
            }
        }
        if let Some(max) = max {
            if len > max {
                self.err(path, format!(".size: expected at most {max}, found {len}"));
            }
        }
    }

    fn check_range(&mut self, value: &Value, min: Option<i128>, max: Option<i128>, inclusive: bool, path: &mut Vec<PathSeg>) {
        let n: Option<i128> = match value {
            Value::Uint(u) => Some(*u as i128),
            Value::Int(i)  => Some(*i as i128),
            Value::Float(f) => Some(*f as i128),
            _ => None,
        };
        let Some(n) = n else {
            self.err(path, format!("expected a number, found {}", kind_name(value)));
            return;
        };
        // The lower bound of a CDDL range (`lo..hi` / `lo...hi`) is always
        // inclusive per RFC 8610 — only the upper bound's inclusivity varies.
        if let Some(min) = min {
            if n < min {
                self.err(path, format!("value {n} is below the minimum {min}"));
                return;
            }
        }
        if let Some(max) = max {
            let over = if inclusive { n > max } else { n >= max };
            if over {
                self.err(path, format!("value {n} is above the maximum {max}"));
            }
        }
    }

    fn check_range_f64(&mut self, value: &Value, min: Option<f64>, max: Option<f64>, inclusive: bool, path: &mut Vec<PathSeg>) {
        let n: Option<f64> = match value {
            Value::Uint(u) => Some(*u as f64),
            Value::Int(i)  => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        };
        let Some(n) = n else {
            self.err(path, format!("expected a number, found {}", kind_name(value)));
            return;
        };
        if let Some(min) = min {
            if n < min {
                self.err(path, format!("value {n} is below the minimum {min}"));
                return;
            }
        }
        if let Some(max) = max {
            let over = if inclusive { n > max } else { n >= max };
            if over {
                self.err(path, format!("value {n} is above the maximum {max}"));
            }
        }
    }

    fn check_regexp(&mut self, value: &Value, pattern: &str, path: &mut Vec<PathSeg>) {
        let Value::Text(s) = value else {
            self.err(path, format!(".regexp constraint does not apply to {}", kind_name(value)));
            return;
        };
        let re = match regex::Regex::new(pattern) {
            Ok(re) => re,
            Err(e) => {
                self.err(path, format!("schema error: invalid .regexp pattern '{pattern}': {e}"));
                return;
            }
        };
        if !re.is_match(s) {
            self.err(path, format!("'{s}' does not match pattern /{pattern}/"));
        }
    }

    fn check_cbor_embedded(&mut self, value: &Value, inner: &TypeRef, is_seq: bool, path: &mut Vec<PathSeg>) {
        let bytes: Vec<u8> = match value {
            Value::Bytes(b) => b.clone(),
            Value::Text(s) => match decode_base64url(s) {
                Some(b) => b,
                None => {
                    self.err(path, "expected base64url-encoded bytes for embedded CBOR".to_owned());
                    return;
                }
            },
            _ => {
                self.err(path, format!(".cbor/.cborseq constraint does not apply to {}", kind_name(value)));
                return;
            }
        };

        if !is_seq {
            match ciborium::from_reader::<ciborium::Value, _>(bytes.as_slice()) {
                Ok(v) => self.check_typeref(inner, &Value::from(v), path),
                Err(e) => self.err(path, format!("embedded CBOR did not decode: {e}")),
            }
            return;
        }

        let mut cursor = bytes.as_slice();
        let mut i = 0usize;
        while !cursor.is_empty() {
            match ciborium::from_reader::<ciborium::Value, _>(&mut cursor) {
                Ok(v) => {
                    path.push(PathSeg::Index(i));
                    self.check_typeref(inner, &Value::from(v), path);
                    path.pop();
                    i += 1;
                }
                Err(e) => {
                    self.err(path, format!("embedded CBOR sequence item {i} did not decode: {e}"));
                    break;
                }
            }
        }
    }
}

fn check_cardinality(occ: &Occurrence, len: usize, name: &str, ctx: &mut Ctx, path: &mut Vec<PathSeg>) {
    match occ {
        Occurrence::Required | Occurrence::Optional | Occurrence::ZeroOrMore { .. } => {}
        Occurrence::OneOrMore { .. } => {
            if len < 1 {
                ctx.err(path, format!("'{name}' requires at least 1 element, found 0"));
            }
        }
        Occurrence::Bounded { min, max, .. } => {
            if (len as u32) < *min {
                ctx.err(path, format!("'{name}' requires at least {min} element(s), found {len}"));
            } else if let Some(max) = max {
                if (len as u32) > *max {
                    ctx.err(path, format!("'{name}' allows at most {max} element(s), found {len}"));
                }
            }
        }
    }
}

fn field_key_matches(field_key: &FieldKey, entry_key: &Value) -> bool {
    match (field_key, entry_key) {
        (FieldKey::Text(name), Value::Text(k)) => name.as_str() == k.as_str(),
        (FieldKey::Int(n), Value::Int(k)) => n == k,
        (FieldKey::Int(n), Value::Uint(k)) => *n >= 0 && (*n as u64) == *k,
        (FieldKey::Uint(n), Value::Uint(k)) => n == k,
        (FieldKey::Uint(n), Value::Int(k)) => *k >= 0 && (*k as u64) == *n,
        _ => false,
    }
}

fn literal_eq(lit: &LiteralValue, value: &Value) -> bool {
    match (lit, value) {
        (LiteralValue::Bool(a), Value::Bool(b)) => a == b,
        (LiteralValue::Null, Value::Null) => true,
        (LiteralValue::Uint(a), Value::Uint(b)) => a == b,
        (LiteralValue::Uint(a), Value::Int(b)) => *b >= 0 && (*b as u64) == *a,
        (LiteralValue::Int(a), Value::Int(b)) => a == b,
        (LiteralValue::Int(a), Value::Uint(b)) => *a >= 0 && (*a as u64) == *b,
        (LiteralValue::Float(a), Value::Float(b)) => a == b,
        (LiteralValue::Text(a), Value::Text(b)) => a == b,
        (LiteralValue::Bytes(a), Value::Bytes(b)) => a == b,
        (LiteralValue::Bytes(a), Value::Text(b)) => decode_base64url(b).is_some_and(|d| &d == a),
        _ => false,
    }
}

fn decode_base64url(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok()
}

fn describe_key(v: &Value) -> String {
    match v {
        Value::Text(s) => format!("'{s}'"),
        other => kind_name(other).to_owned(),
    }
}

fn describe_literal(lit: &LiteralValue) -> String {
    match lit {
        LiteralValue::Bool(b)  => b.to_string(),
        LiteralValue::Null     => "null".to_owned(),
        LiteralValue::Uint(n)  => n.to_string(),
        LiteralValue::Int(n)   => n.to_string(),
        LiteralValue::Float(f) => f.to_string(),
        LiteralValue::Bytes(_) => "<bytes>".to_owned(),
        LiteralValue::Text(s)  => format!("'{s}'"),
    }
}

fn kind_name(v: &Value) -> &'static str {
    match v {
        Value::Null       => "null",
        Value::Bool(_)    => "bool",
        Value::Uint(_)    => "uint",
        Value::Int(_)     => "int",
        Value::Float(_)   => "float",
        Value::Text(_)    => "text",
        Value::Bytes(_)   => "bytes",
        Value::Array(_)   => "array",
        Value::Map(_)     => "map",
        Value::Tag(..)    => "tagged value",
    }
}

fn primitive_name(p: &Primitive) -> &'static str {
    match p {
        Primitive::Bool      => "bool",
        Primitive::Null      => "null",
        Primitive::Undefined => "undefined",
        Primitive::Uint      => "uint",
        Primitive::Int       => "int",
        Primitive::Float16   => "float16",
        Primitive::Float32   => "float32",
        Primitive::Float64   => "float64",
        Primitive::Float     => "float",
        Primitive::Bstr      => "bstr",
        Primitive::Tstr      => "tstr",
        Primitive::Any       => "any",
    }
}
