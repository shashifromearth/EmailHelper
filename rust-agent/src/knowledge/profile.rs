//! The user's profile assembled from the JSON files in the knowledge directory.
//! Every file is optional; missing fields default to empty.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Identity {
    pub full_name: String,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub phone: String,
    pub location: String,
    pub city: String,
    pub country: String,
    pub linkedin_url: String,
    pub github_url: String,
    pub portfolio_url: String,
    pub work_authorization: String,
    pub requires_sponsorship: Option<bool>,
    pub notice_period: String,
    pub willing_to_relocate: Option<bool>,
    pub current_company: String,
    pub current_title: String,
    pub signature: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Resume {
    pub headline: String,
    pub summary: String,
    pub total_experience_years: f64,
    pub resume_file: String,
    pub achievements: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Skills {
    pub core: Vec<String>,
    pub secondary: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SalaryPreferences {
    pub currency: String,
    pub minimum_annual: Option<f64>,
    pub expected_annual: Option<f64>,
    pub expected_text: String,
    pub current_text: String,
    pub negotiable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// remote_only | hybrid | onsite | any
    pub remote: String,
    pub preferred_locations: Vec<String>,
    pub target_roles: Vec<String>,
    pub blacklisted_companies: Vec<String>,
    pub visa_sponsorship_required: bool,
    pub auto_accept_application_consent: bool,
    pub how_did_you_hear_default: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Employment {
    pub company: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub location: String,
    pub highlights: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Education {
    pub institution: String,
    pub degree: String,
    pub field: String,
    pub year: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub name: String,
    pub description: String,
    pub tech: Vec<String>,
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Certification {
    pub name: String,
    pub issuer: String,
    pub year: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Profile {
    pub identity: Identity,
    pub resume: Resume,
    pub skills: Skills,
    pub salary: SalaryPreferences,
    pub preferences: Preferences,
    pub employment: Vec<Employment>,
    pub education: Vec<Education>,
    pub projects: Vec<Project>,
    pub certifications: Vec<Certification>,
    pub company_profile: String,
    pub tone: String,
}

fn read_json<T: serde::de::DeserializeOwned + Default>(dir: &Path, name: &str) -> T {
    let p = dir.join(name);
    match std::fs::read_to_string(&p) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(file = %p.display(), error = %e, "invalid knowledge JSON; ignoring");
                T::default()
            }
        },
        Err(_) => T::default(),
    }
}

impl Profile {
    pub fn load(dir: &Path) -> Self {
        let mut identity: Identity = read_json(dir, "profile.json");
        if identity.full_name.is_empty() && !identity.first_name.is_empty() {
            identity.full_name = format!("{} {}", identity.first_name, identity.last_name)
                .trim()
                .to_string();
        }
        if identity.first_name.is_empty() && !identity.full_name.is_empty() {
            let mut parts = identity.full_name.split_whitespace();
            identity.first_name = parts.next().unwrap_or("").to_string();
            identity.last_name = parts.collect::<Vec<_>>().join(" ");
        }
        let employment: Vec<Employment> = read_json(dir, "employment_history.json");
        if identity.current_company.is_empty() {
            if let Some(e) = employment.first() {
                identity.current_company = e.company.clone();
                if identity.current_title.is_empty() {
                    identity.current_title = e.title.clone();
                }
            }
        }
        Self {
            identity,
            resume: read_json(dir, "resume.json"),
            skills: read_json(dir, "skills.json"),
            salary: read_json(dir, "salary_preferences.json"),
            preferences: read_json(dir, "preferences.json"),
            employment,
            education: read_json(dir, "education.json"),
            projects: read_json(dir, "projects.json"),
            certifications: read_json(dir, "certifications.json"),
            company_profile: std::fs::read_to_string(dir.join("company_profile.md"))
                .unwrap_or_default(),
            tone: std::fs::read_to_string(dir.join("tone.md")).unwrap_or_default(),
        }
    }

    pub fn display_name(&self) -> &str {
        if self.identity.full_name.is_empty() {
            "the user"
        } else {
            &self.identity.full_name
        }
    }

    pub fn signature(&self) -> String {
        if !self.identity.signature.is_empty() {
            return self.identity.signature.clone();
        }
        if self.identity.full_name.is_empty() {
            "Best regards".to_string()
        } else {
            format!("Best regards,\n{}", self.identity.full_name)
        }
    }

    pub fn all_skills(&self) -> Vec<String> {
        let mut v = self.skills.core.clone();
        v.extend(self.skills.secondary.iter().cloned());
        for p in &self.projects {
            for t in &p.tech {
                if !v.iter().any(|s| s.eq_ignore_ascii_case(t)) {
                    v.push(t.clone());
                }
            }
        }
        v
    }

    pub fn highest_education(&self) -> Option<&Education> {
        self.education.first()
    }

    /// Short plain-text summary used inside prompts (cover letters, answers).
    pub fn summary_text(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("Name: {}\n", self.display_name()));
        if !self.resume.headline.is_empty() {
            s.push_str(&format!("Headline: {}\n", self.resume.headline));
        }
        if self.resume.total_experience_years > 0.0 {
            s.push_str(&format!(
                "Total experience: {} years\n",
                self.resume.total_experience_years
            ));
        }
        if !self.skills.core.is_empty() {
            s.push_str(&format!("Core skills: {}\n", self.skills.core.join(", ")));
        }
        if !self.resume.summary.is_empty() {
            s.push_str(&format!("Summary: {}\n", self.resume.summary));
        }
        for e in self.employment.iter().take(3) {
            s.push_str(&format!(
                "- {} at {} ({} – {}): {}\n",
                e.title,
                e.company,
                e.start,
                if e.end.is_empty() { "present" } else { &e.end },
                e.highlights.join("; ")
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_sample_knowledge() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../knowledge");
        let p = Profile::load(&dir);
        assert!(!p.identity.full_name.is_empty());
        assert!(!p.identity.first_name.is_empty());
        assert!(!p.skills.core.is_empty());
        assert!(!p.preferences.target_roles.is_empty());
        assert!(p.signature().contains(&p.identity.first_name) || !p.identity.signature.is_empty());
    }

    #[test]
    fn missing_dir_is_fine() {
        let p = Profile::load(Path::new("definitely/not/here"));
        assert_eq!(p.display_name(), "the user");
        assert_eq!(p.signature(), "Best regards");
    }
}
