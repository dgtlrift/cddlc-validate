use std::fmt;

/// One step in the breadcrumb trail from the document root to where a
/// validation failure occurred.
#[derive(Debug, Clone, PartialEq)]
pub enum PathSeg {
    /// A named struct field, e.g. `.readings`
    Field(String),
    /// An array index, e.g. `[3]`
    Index(usize),
    /// A map/table entry key, rendered as `{key}`
    Key(String),
    /// An enum (type choice) variant attempt, rendered as `<variant>`
    Variant(String),
}

impl fmt::Display for PathSeg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSeg::Field(name) => write!(f, ".{name}"),
            PathSeg::Index(i) => write!(f, "[{i}]"),
            PathSeg::Key(k) => write!(f, "{{{k}}}"),
            PathSeg::Variant(v) => write!(f, "<{v}>"),
        }
    }
}

/// A single validation failure, with a breadcrumb path back to the document
/// root (e.g. `readings[3].value`).
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub path:    Vec<PathSeg>,
    pub message: String,
}

impl ValidationError {
    pub fn new(path: Vec<PathSeg>, message: impl Into<String>) -> Self {
        Self { path, message: message.into() }
    }

    pub fn path_string(&self) -> String {
        if self.path.is_empty() {
            return "$".to_owned();
        }
        let mut s = String::from("$");
        for seg in &self.path {
            s.push_str(&seg.to_string());
        }
        s
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path_string(), self.message)
    }
}

impl std::error::Error for ValidationError {}
