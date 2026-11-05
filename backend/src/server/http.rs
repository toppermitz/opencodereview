use super::*;

pub(crate) fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    let mut content_length = 0usize;
    loop {
        let n = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if header_end.is_none() {
            header_end = find_header_end(&buffer);
            if let Some(end) = header_end {
                let headers = String::from_utf8_lossy(&buffer[..end]);
                content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
            }
        }
        if let Some(end) = header_end {
            if buffer.len() >= end + 4 + content_length {
                break;
            }
        }
    }
    let end = header_end.ok_or("invalid HTTP request")?;
    let head = String::from_utf8_lossy(&buffer[..end]);
    let first = head.lines().next().ok_or("missing request line")?;
    let origin = head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("origin").then(|| value.trim().to_string())
    });
    let authorization = head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_string())
    });
    let content_type = head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("content-type")
            .then(|| value.trim().to_string())
    });
    let flow_secret = head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("x-flow-secret")
            .then(|| value.trim().to_string())
    });
    let mut parts = first.split_whitespace();
    let method = parts.next().ok_or("missing method")?.to_string();
    let raw_path = parts.next().ok_or("missing path")?;
    let (path, query) = raw_path.split_once('?').unwrap_or((raw_path, ""));
    let (path, query) = (path.to_string(), query.to_string());
    let body_start = end + 4;
    let body_end = (body_start + content_length).min(buffer.len());
    Ok(Request {
        method,
        path,
        query,
        origin,
        authorization,
        content_type,
        flow_secret,
        body: buffer[body_start..body_end].to_vec(),
    })
}

pub(crate) fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

pub(crate) fn is_public_route(method: &str, path: &str) -> bool {
    matches!(
        (method, path),
        ("GET", "/health")
            | ("GET", "/auth/gitlab/status")
            | ("POST", "/auth/gitlab/start")
            | ("POST", "/auth/gitlab/cancel")
            | ("GET", "/auth/gitlab/events")
            | ("GET", "/auth/claude/status")
            | ("POST", "/auth/claude/start")
            | ("GET", "/auth/claude/events")
            | ("POST", "/auth/claude/input")
            | ("POST", "/auth/claude/cancel")
    )
}

pub(crate) fn check_auth(request: &Request, state: &SharedState) -> Option<Response> {
    let token = match &state.config.api_token {
        Some(t) => t,
        None => {
            return Some(json_error(
                401,
                "unauthorized: OCR_API_TOKEN must be set to use this endpoint",
            ));
        }
    };
    let provided = request.authorization.as_deref().and_then(|h| h.strip_prefix("Bearer "));
    match provided {
        Some(value) if value == token => None,
        _ => Some(json_error(401, "unauthorized: invalid or missing API token")),
    }
}

pub(crate) fn has_json_content_type(request: &Request) -> bool {
    request
        .content_type
        .as_deref()
        .map(|ct| ct.split(';').next().unwrap_or("").trim())
        .is_some_and(|mime| mime.eq_ignore_ascii_case("application/json"))
}
