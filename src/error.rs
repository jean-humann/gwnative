use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The patch service answered, but with a status we will not retry past.
    Http {
        url: String,
        status: u16,
    },
    Transport {
        url: String,
        detail: String,
    },
    /// A response exceeded the byte budget declared for it.
    TooLarge {
        url: String,
        limit: u64,
    },
    ManifestFormat(String),
    HashFormat(String),
    HashMismatch {
        expected: String,
        actual: String,
    },
    Decode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Http { url, status } => write!(f, "{url} returned HTTP {status}"),
            Self::Transport { url, detail } => write!(f, "{url}: {detail}"),
            Self::TooLarge { url, limit } => write!(f, "{url} exceeded {limit} bytes"),
            Self::ManifestFormat(m) => write!(f, "manifest: {m}"),
            Self::HashFormat(h) => write!(f, "unsupported content hash: {h}"),
            Self::HashMismatch { expected, actual } => {
                write!(f, "chunk hash mismatch: expected {expected}, got {actual}")
            }
            Self::Decode(m) => write!(f, "chunk decode failed: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
