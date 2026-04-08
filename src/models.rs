use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StudentType {
    Undergrad,
    Grad,
}

impl StudentType {
    pub fn as_code(&self) -> &'static str {
        match self {
            Self::Undergrad => "1",
            Self::Grad => "2",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EasToken {
    pub cookies: HashMap<String, String>,
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
    pub name: Option<String>,
    pub stutype: StudentType,
    pub picture: Option<String>,
    pub id: Option<String>,
    pub stu_id: Option<String>,
    pub school: Option<String>,
    pub major: Option<String>,
    pub grade: Option<String>,
    pub sfxsx: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractedLink {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageSnapshot {
    pub final_url: String,
    pub title: Option<String>,
    pub links: Vec<ExtractedLink>,
    pub html: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthenticatedFetchResult {
    pub token: EasToken,
    pub fetched_page: PageSnapshot,
}
