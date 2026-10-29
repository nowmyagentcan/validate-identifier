//! The MCP server on stdio: one tool, answering with the CLI's `--json` object.

use std::sync::Arc;

use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use rmcp::service::ServerInitializeError;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::generate::SchemaSettings;
use schemars::transform::RecursiveTransform;
use schemars::{JsonSchema, Schema};
use serde_json::Value;

use crate::check::{self, Options, Report, Request, failure, usage};
use crate::item::Error;

/// Values per call, and per call with live checks (spec §2.3).
const MAX_VALUES: usize = 1_000;
const MAX_LIVE: usize = 100;

#[derive(Clone)]
struct Server;

#[tool_router]
impl Server {
    #[tool(
        description = "Check identifiers and say whether they are valid: IBAN (registry format and check digits); VAT numbers for the EU, Northern Ireland (XI), the UK and Switzerland (format and check digits, or live from VIES, HMRC or the Swiss UID register, with the registered name); phone numbers (valid, mobile or fixed line, region, E.164 and national formats); email addresses (syntax, and mail servers when live); LEI (GLEIF registration status); Australian ABN (ABN Lookup); Swiss UID; EORI (EU EOS, HMRC); Peppol participants (is it registered on Peppol, which document types it accepts; also accepts a VAT number, ABN, UID or LEI). The type is detected when omitted. Offline by default; live true asks the registries over the network. A registry outage gives status unavailable or rate_limited, never invalid, and the offline result is still returned. UK VAT and ABN live checks need the user's own credentials in environment variables; without them the status is not_configured. Bad arguments (no values, too many, an unknown type or argument) give isError true and one text block holding {\"error\":{\"code\",\"message\",\"hint\"}}, hint null when there is none; an invalid identifier is a normal result, so read each result's status.",
        input_schema = input(),
        output_schema = schema::<Report>(SchemaSettings::draft2020_12().for_serialize()),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn validate_identifiers(&self, args: JsonObject) -> CallToolResult {
        let answer = match read(args) {
            // Live checks block on HTTP and run their own DNS runtime, so off this one.
            Ok((values, o)) => tokio::task::spawn_blocking(move || check::run(&values, &o))
                .await
                .expect("checks don't panic"),
            Err(e) => Err(e),
        };
        match answer {
            Ok(r) => CallToolResult::structured(serde_json::to_value(r).expect("plain data")),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(failure(&e))]),
        }
    }
}

#[tool_handler(
    name = "validate-identifier",
    instructions = "Validate IBANs, VAT numbers, phone numbers, email addresses, LEIs, Australian ABNs, Swiss UIDs, EORI numbers and Peppol participant IDs. Offline checks are instant; set live to true to confirm with the official registry (VIES, HMRC, the Swiss UID register, ABN Lookup, GLEIF, EOS, the Peppol SML, DNS). Use this instead of writing check-digit code or calling registries yourself."
)]
impl ServerHandler for Server {}

/// The arguments as a run's values and options; a call the schema or the limits refuse is an
/// error.
fn read(args: JsonObject) -> Result<(Vec<String>, Options), Error> {
    let r: Request = serde_json::from_value(Value::Object(args)).map_err(|e| {
        Error::new(
            "bad_request",
            format!("Can't read the arguments: {e}."),
            None,
        )
    })?;
    let n = r.values.len();
    if n == 0 {
        return Err(usage("No values to check.", "Pass at least one value."));
    }
    if n > MAX_VALUES {
        let message = format!("{n} values; at most 1,000 per call.");
        return Err(usage(message, "Split the list."));
    }
    // The schema's 1-256 characters per value, for clients that don't check it.
    if let Some(i) = r.values.iter().position(|v| v.is_empty()) {
        let message = format!("Value {} is empty.", i + 1);
        return Err(usage(message, "Remove it, or pass the value."));
    }
    if let Some(i) = r.values.iter().position(|v| v.chars().count() > 256) {
        let message = format!("Value {} is longer than 256 characters.", i + 1);
        return Err(usage(message, "Check one identifier per value."));
    }
    if r.live && n > MAX_LIVE {
        let message = format!("{n} values; at most 100 per call with live true.");
        return Err(usage(message, "Split the list, or set live to false."));
    }
    let o = r.options()?;
    Ok((r.values, o))
}

/// The tool's arguments, closed to names it doesn't define.
fn input() -> Arc<JsonObject> {
    let mut schema = schema::<Request>(SchemaSettings::draft2020_12());
    Arc::make_mut(&mut schema).insert("additionalProperties".into(), false.into());
    schema
}

/// A schema with an object at its root, as 2025-11-25 clients require, and without the Rust
/// number formats schemars adds (uint, uint64), which JSON Schema doesn't define.
fn schema<T: JsonSchema>(settings: SchemaSettings) -> Arc<JsonObject> {
    let no_format = |s: &mut Schema| drop(s.remove("format"));
    let schema = settings
        .with_transform(RecursiveTransform(no_format))
        .into_generator()
        .into_root_schema_for::<T>();
    let Value::Object(mut schema) = schema.to_value() else {
        unreachable!("a root schema is an object")
    };
    schema.remove("title");
    schema.remove("description");
    schema.insert("type".into(), "object".into());
    Arc::new(schema)
}

/// Serves until the client closes stdin.
pub fn serve() -> Result<(), Error> {
    let io =
        |e: &dyn std::fmt::Display| Error::new("io", format!("Couldn't read input: {e}."), None);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| io(&e))?;
    let served = runtime.block_on(async {
        match Server.serve(rmcp::transport::stdio()).await {
            Ok(service) => service.waiting().await.map(drop).map_err(|e| io(&e)),
            // stdin closed before the handshake ended: the client left, which is no failure.
            Err(ServerInitializeError::ConnectionClosed(_)) => Ok(()),
            // stdout closed before the answer went out: the client left too.
            Err(ServerInitializeError::TransportError { error, .. })
                if error
                    .error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) =>
            {
                Ok(())
            }
            Err(e) => Err(io(&e)),
        }
    });
    // A check still running for a client that left isn't waited for.
    runtime.shutdown_background();
    served
}
