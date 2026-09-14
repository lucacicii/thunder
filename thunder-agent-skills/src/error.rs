#[derive(Debug)]
pub enum SkillError {
    IoError(String),
    ParseError(String),
    NotFound(String),
    InvalidFrontmatter(String),
    InvalidFormat(String),
    RegistrationError(String),
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(msg) => write!(f, "Skill IO error: {msg}"),
            Self::ParseError(msg) => write!(f, "Skill parse error: {msg}"),
            Self::NotFound(msg) => write!(f, "Skill not found: {msg}"),
            Self::InvalidFrontmatter(msg) => write!(f, "Invalid frontmatter: {msg}"),
            Self::InvalidFormat(msg) => write!(f, "Invalid skill format: {msg}"),
            Self::RegistrationError(msg) => write!(f, "Skill registration error: {msg}"),
        }
    }
}

impl std::error::Error for SkillError {}

impl From<std::io::Error> for SkillError {
    fn from(err: std::io::Error) -> Self {
        Self::IoError(err.to_string())
    }
}

impl From<serde_json::Error> for SkillError {
    fn from(err: serde_json::Error) -> Self {
        Self::ParseError(err.to_string())
    }
}

impl From<serde_yaml::Error> for SkillError {
    fn from(err: serde_yaml::Error) -> Self {
        Self::InvalidFrontmatter(err.to_string())
    }
}
