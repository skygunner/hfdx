use super::fixture::Fixture;
use serde_json::Value;

impl Fixture {
    pub async fn control(&self, value: Value) {
        assert_eq!(
            self.http
                .post(format!("{}/__control", self.origin()))
                .json(&value)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    pub async fn event_status(&self, path: &str, status: u16) -> Value {
        let mut after = 0;
        loop {
            let state: Value = self
                .http
                .get(format!("{}/__state", self.origin()))
                .query(&[
                    ("kind", "complete"),
                    ("path", path),
                    ("role", "origin"),
                    ("timeout", "5"),
                    ("after", &after.to_string()),
                ])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let events = state["events"].as_array().unwrap();
            assert!(!events.is_empty(), "expected fixture status {status}");
            if let Some(event) = events.iter().find(|event| event["status"] == status) {
                return event.clone();
            }
            after = events.last().unwrap()["seq"].as_u64().unwrap();
        }
    }
}
