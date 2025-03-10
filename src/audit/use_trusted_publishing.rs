use std::{collections::HashMap, ops::Deref};

use github_actions_models::{
    common::EnvValue,
    workflow::{job::StepBody, Job},
};

use super::WorkflowAudit;
use crate::{
    finding::{Confidence, Severity},
    state::AuditState,
};

const USES_MANUAL_CREDENTIAL: &str =
    "uses a manually-configured credential instead of Trusted Publishing";

const KNOWN_PYTHON_TP_INDICES: &[&str] = &[
    "https://upload.pypi.org/legacy/",
    "https://test.pypi.org/legacy/",
];

pub(crate) struct UseTrustedPublishing {
    pub(crate) _state: AuditState,
}

impl UseTrustedPublishing {
    fn pypi_publish_uses_manual_credentials(&self, with: &HashMap<String, EnvValue>) -> bool {
        // `password` implies the step isn't using Trusted Publishing,
        // but we also need to check `repository-url` to prevent false-positives
        // on third-party indices.
        let has_manual_credential = with.contains_key("password");

        match with
            .get("repository-url")
            .or_else(|| with.get("repository_url"))
        {
            Some(repo_url) => {
                has_manual_credential
                    && KNOWN_PYTHON_TP_INDICES.contains(&repo_url.to_string().as_str())
            }
            None => has_manual_credential,
        }
    }

    fn release_gem_uses_manual_credentials(&self, with: &HashMap<String, EnvValue>) -> bool {
        match with.get("setup-trusted-publisher") {
            Some(v) if v.to_string() == "true" => false,
            // Anything besides `true` means to *not* use trusted publishing.
            Some(_) => true,
            // Not set means the default, which is trusted publishing.
            None => false,
        }
    }

    fn rubygems_credential_uses_manual_credentials(
        &self,
        with: &HashMap<String, EnvValue>,
    ) -> bool {
        with.contains_key("api-token")
    }
}

impl WorkflowAudit for UseTrustedPublishing {
    fn ident() -> &'static str {
        "use-trusted-publishing"
    }

    fn desc() -> &'static str
    where
        Self: Sized,
    {
        "prefer trusted publishing for authentication"
    }

    fn new(state: AuditState) -> anyhow::Result<Self> {
        Ok(Self { _state: state })
    }

    fn audit<'w>(
        &self,
        workflow: &'w crate::models::Workflow,
    ) -> anyhow::Result<Vec<crate::finding::Finding<'w>>> {
        let mut findings = vec![];

        for job in workflow.jobs() {
            if !matches!(job.deref(), Job::NormalJob(_)) {
                continue;
            }

            for step in job.steps() {
                let StepBody::Uses { uses, with } = &step.deref().body else {
                    continue;
                };

                if uses.starts_with("pypa/gh-action-pypi-publish") {
                    if self.pypi_publish_uses_manual_credentials(with) {
                        findings.push(
                            Self::finding()
                                .severity(Severity::Informational)
                                .confidence(Confidence::High)
                                .add_location(
                                    step.location()
                                        .with_keys(&["uses".into()])
                                        .annotated("this step"),
                                )
                                .add_location(
                                    step.location()
                                        .with_keys(&["with".into(), "password".into()])
                                        .annotated(USES_MANUAL_CREDENTIAL),
                                )
                                .build(workflow)?,
                        );
                    }
                } else if uses.starts_with("rubygems/release-gem") {
                    if self.release_gem_uses_manual_credentials(with) {
                        findings.push(
                            Self::finding()
                                .severity(Severity::Informational)
                                .confidence(Confidence::High)
                                .add_location(
                                    step.location()
                                        .with_keys(&["uses".into()])
                                        .annotated("this step"),
                                )
                                .add_location(step.location().annotated(USES_MANUAL_CREDENTIAL))
                                .build(workflow)?,
                        );
                    }
                } else if uses.starts_with("rubygems/configure-rubygems-credential")
                    && self.rubygems_credential_uses_manual_credentials(with)
                {
                    findings.push(
                        Self::finding()
                            .severity(Severity::Informational)
                            .confidence(Confidence::High)
                            .add_location(
                                step.location()
                                    .with_keys(&["uses".into()])
                                    .annotated("this step"),
                            )
                            .add_location(step.location().annotated(USES_MANUAL_CREDENTIAL))
                            .build(workflow)?,
                    );
                }
            }
        }

        Ok(findings)
    }
}
