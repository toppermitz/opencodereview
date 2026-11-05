use crate::gitlab::{StatusComment, StatusCommentApi};

const PUBLISHER: i64 = 77;

/// In-memory `StatusCommentApi` with call counters, failure injection and
/// an optional per-PR list failure.
pub(crate) struct FakeStatusComments {
    pub(crate) next_id: i64,
    pub(crate) comments: Vec<StatusComment>,
    pub(crate) posts: usize,
    pub(crate) deletes: usize,
    pub(crate) post_writes_then_fails: bool,
    pub(crate) fail_post: bool,
    pub(crate) fail_delete: bool,
    pub(crate) fail_list_for: Option<i64>,
}

impl FakeStatusComments {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            comments: Vec::new(),
            posts: 0,
            deletes: 0,
            post_writes_then_fails: false,
            fail_post: false,
            fail_delete: false,
            fail_list_for: None,
        }
    }

    pub(crate) fn add(&mut self, author_id: i64, body: &str) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.comments.push(StatusComment {
            id,
            author_id,
            body: body.to_string(),
        });
        id
    }

    pub(crate) fn own(&mut self, body: &str) -> i64 {
        self.add(PUBLISHER, body)
    }

    pub(crate) fn ids(&self) -> Vec<i64> {
        self.comments.iter().map(|comment| comment.id).collect()
    }
}

impl StatusCommentApi for FakeStatusComments {
    fn publisher_id(&mut self) -> Result<i64, String> {
        Ok(PUBLISHER)
    }

    fn list(&mut self, _repo: &str, number: i64) -> Result<Vec<StatusComment>, String> {
        if self.fail_list_for == Some(number) {
            return Err("list failed".to_string());
        }
        Ok(self.comments.clone())
    }

    fn post(&mut self, _repo: &str, _number: i64, body: &str) -> Result<i64, String> {
        self.posts += 1;
        if self.fail_post {
            return Err("post failed".to_string());
        }
        let id = self.add(PUBLISHER, body);
        if self.post_writes_then_fails {
            return Err("post response lost".to_string());
        }
        Ok(id)
    }

    fn delete(&mut self, _repo: &str, _number: i64, id: i64) -> Result<(), String> {
        self.deletes += 1;
        if self.fail_delete {
            return Err("delete failed".to_string());
        }
        self.comments.retain(|comment| comment.id != id);
        Ok(())
    }
}
