//! Test doubles shared by the unit tests.

use std::sync::Mutex;

use crate::{ForgeError, Http, HttpResponse};

pub(crate) type Seen = Vec<(String, Vec<(String, String)>)>;

/// Answers requests from a script, recording what was asked.
#[derive(Debug, Default)]
pub(crate) struct Scripted {
    answers: Mutex<Vec<Result<HttpResponse, ForgeError>>>,
    pub(crate) seen: Mutex<Seen>,
}

impl Scripted {
    pub(crate) fn new(answers: Vec<Result<HttpResponse, ForgeError>>) -> Self {
        let mut answers = answers;
        answers.reverse();
        Self {
            answers: Mutex::new(answers),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn urls(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(url, _)| url.clone())
            .collect()
    }
}

impl Http for Scripted {
    /// Recorded with the method and the body after the URL.
    fn post(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse, ForgeError> {
        self.get(&format!("POST {url} {body}"), headers)
    }

    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<HttpResponse, ForgeError> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((
                url.to_owned(),
                headers
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
            ));
        self.answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
            .unwrap_or_else(|| Ok(reply(404, "")))
    }
}

pub(crate) fn reply(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: Vec::new(),
        body: body.as_bytes().to_vec(),
    }
}
