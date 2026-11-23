use std::io::Write;

use serde::{Deserialize, Serialize};

pub(super) const RESP_PREFIX: &str = "MS_LOK_RESP:";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub(super) enum WorkerRequest {
    Convert {
        src_path: String,
        out_pdf_path: String,
        page_range: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum WorkerResponse {
    Ready,
    InitFailed { error: String },
    Ok,
    Err { error: String },
}

fn write_json_line<T: Serialize>(
    writer: &mut impl Write,
    prefix: &str,
    value: &T,
) -> std::io::Result<()> {
    if !prefix.is_empty() {
        writer.write_all(prefix.as_bytes())?;
    }
    serde_json::to_writer(&mut *writer, value).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub(super) fn write_request(writer: &mut impl Write, req: &WorkerRequest) -> std::io::Result<()> {
    write_json_line(writer, "", req)
}

pub(super) fn write_response(
    writer: &mut impl Write,
    resp: &WorkerResponse,
) -> std::io::Result<()> {
    write_json_line(writer, RESP_PREFIX, resp)
}

pub(super) fn parse_response_line(line: &str) -> Option<Result<WorkerResponse, serde_json::Error>> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let payload = trimmed.strip_prefix(RESP_PREFIX)?;
    Some(serde_json::from_str(payload))
}

pub(super) fn build_page_range_filter_options(range: &str) -> String {
    serde_json::json!({
        "PageRange": {
            "type": "string",
            "value": range
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefixed_responses_and_skips_stdout_noise() {
        assert!(parse_response_line("warn:sal.osl: something noisy\n").is_none());
        assert!(parse_response_line("   \r\n").is_none());

        assert_eq!(
            parse_response_line("MS_LOK_RESP:{\"status\":\"ready\"}\n")
                .expect("prefixed line")
                .expect("valid json"),
            WorkerResponse::Ready
        );
        assert_eq!(
            parse_response_line(
                "MS_LOK_RESP:{\"status\":\"init_failed\",\"error\":\"no lib\"}\r\n"
            )
            .expect("prefixed line")
            .expect("valid json"),
            WorkerResponse::InitFailed {
                error: "no lib".to_string(),
            }
        );
        assert_eq!(
            parse_response_line("MS_LOK_RESP:{\"status\":\"ok\"}\n")
                .expect("prefixed line")
                .expect("valid json"),
            WorkerResponse::Ok
        );
        assert_eq!(
            parse_response_line("MS_LOK_RESP:{\"status\":\"err\",\"error\":\"bad doc\"}\n")
                .expect("prefixed line")
                .expect("valid json"),
            WorkerResponse::Err {
                error: "bad doc".to_string(),
            }
        );
    }

    #[test]
    fn builds_page_range_filter_options_json() {
        let json = build_page_range_filter_options("1");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            parsed,
            serde_json::json!({
                "PageRange": {
                    "type": "string",
                    "value": "1"
                }
            })
        );
    }

    #[test]
    fn worker_request_json_roundtrip() {
        let convert_req = WorkerRequest::Convert {
            src_path: "/tmp/input.docx".to_string(),
            out_pdf_path: "/tmp/out.pdf".to_string(),
            page_range: Some("1".to_string()),
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &convert_req).expect("serialize convert request");
        let line = String::from_utf8(buf).expect("utf8 json");
        assert!(line.ends_with('\n'));
        let decoded: WorkerRequest =
            serde_json::from_str(line.trim_end()).expect("deserialize convert request");
        assert_eq!(decoded, convert_req);

        let shutdown_req = WorkerRequest::Shutdown;
        let mut shutdown_buf = Vec::new();
        write_request(&mut shutdown_buf, &shutdown_req).expect("serialize shutdown request");
        let decoded_shutdown: WorkerRequest =
            serde_json::from_slice(shutdown_buf.trim_ascii_end()).expect("deserialize shutdown");
        assert_eq!(decoded_shutdown, shutdown_req);

        let mut resp_buf = Vec::new();
        write_response(&mut resp_buf, &WorkerResponse::Ready).expect("serialize response");
        let resp_line = String::from_utf8(resp_buf).expect("utf8 resp");
        assert_eq!(
            parse_response_line(&resp_line)
                .expect("prefixed")
                .expect("valid"),
            WorkerResponse::Ready
        );
    }
}
