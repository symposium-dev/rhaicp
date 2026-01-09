//! Rhai module providing MCP tool access via `mcp::list_tools` and `mcp::call_tool`

use crate::RhaiMessage;
use rhai::{Dynamic, FuncRegistration, Module};
use tokio::sync::mpsc;

/// MCP module for Rhai that provides tool access
pub struct McpModule {
    msg_tx: mpsc::UnboundedSender<RhaiMessage>,
}

impl McpModule {
    pub fn new(msg_tx: mpsc::UnboundedSender<RhaiMessage>) -> Self {
        Self { msg_tx }
    }
}

impl From<McpModule> for Module {
    fn from(mcp: McpModule) -> Self {
        let mut module = Module::new();

        // list_tools(server) -> Array of tool names
        let tx = mcp.msg_tx.clone();
        FuncRegistration::new("list_tools")
            .in_global_namespace()
            .set_into_module(&mut module, move |server: &str| -> Dynamic {
                let (response_tx, response_rx) = std::sync::mpsc::channel();

                let _ = tx.send(RhaiMessage::ListTools {
                    server: server.to_string(),
                    response_tx,
                });

                // Block waiting for the async runtime to respond
                match response_rx.recv() {
                    Ok(Ok(tools)) => {
                        // Convert Vec<String> to Rhai Array
                        tools
                            .into_iter()
                            .map(Dynamic::from)
                            .collect::<Vec<_>>()
                            .into()
                    }
                    Ok(Err(e)) => {
                        // Return error as a string - Rhai can check for this
                        Dynamic::from(format!("ERROR: {}", e))
                    }
                    Err(_) => Dynamic::from("ERROR: Channel closed"),
                }
            });

        // call_tool(server, tool, args) -> Dynamic result
        // args should be a Rhai Map that we convert to JSON
        let tx = mcp.msg_tx.clone();
        FuncRegistration::new("call_tool")
            .in_global_namespace()
            .set_into_module(
                &mut module,
                move |server: &str, tool: &str, args: Dynamic| -> Dynamic {
                    let (response_tx, response_rx) = std::sync::mpsc::channel();

                    // Convert Rhai Dynamic to serde_json::Value
                    let json_args = dynamic_to_json(&args);

                    let _ = tx.send(RhaiMessage::CallTool {
                        server: server.to_string(),
                        tool: tool.to_string(),
                        args: json_args,
                        response_tx,
                    });

                    // Block waiting for the async runtime to respond
                    match response_rx.recv() {
                        Ok(Ok(result)) => {
                            // Convert JSON result back to Dynamic
                            json_to_dynamic(&result)
                        }
                        Ok(Err(e)) => Dynamic::from(format!("ERROR: {}", e)),
                        Err(_) => Dynamic::from("ERROR: Channel closed"),
                    }
                },
            );

        module
    }
}

/// Convert a Rhai Dynamic value to serde_json::Value
fn dynamic_to_json(value: &Dynamic) -> serde_json::Value {
    if value.is_unit() {
        serde_json::Value::Null
    } else if value.is_bool() {
        serde_json::Value::Bool(value.as_bool().unwrap())
    } else if value.is_int() {
        serde_json::Value::Number(value.as_int().unwrap().into())
    } else if value.is_float() {
        if let Some(n) = serde_json::Number::from_f64(value.as_float().unwrap()) {
            serde_json::Value::Number(n)
        } else {
            serde_json::Value::Null
        }
    } else if value.is_string() {
        serde_json::Value::String(value.clone().into_string().unwrap())
    } else if value.is_array() {
        let arr = value.clone().into_array().unwrap();
        serde_json::Value::Array(arr.iter().map(dynamic_to_json).collect())
    } else if value.is_map() {
        let map: rhai::Map = value.clone().cast();
        let obj: serde_json::Map<String, serde_json::Value> = map
            .into_iter()
            .map(|(k, v)| (k.to_string(), dynamic_to_json(&v)))
            .collect();
        serde_json::Value::Object(obj)
    } else {
        // Fallback: try to get string representation
        serde_json::Value::String(format!("{:?}", value))
    }
}

/// Convert a serde_json::Value to Rhai Dynamic
fn json_to_dynamic(value: &serde_json::Value) -> Dynamic {
    match value {
        serde_json::Value::Null => Dynamic::UNIT,
        serde_json::Value::Bool(b) => Dynamic::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from(f)
            } else {
                Dynamic::UNIT
            }
        }
        serde_json::Value::String(s) => Dynamic::from(s.clone()),
        serde_json::Value::Array(arr) => {
            let rhai_arr: Vec<Dynamic> = arr.iter().map(json_to_dynamic).collect();
            Dynamic::from(rhai_arr)
        }
        serde_json::Value::Object(obj) => {
            let mut map = rhai::Map::new();
            for (k, v) in obj {
                map.insert(k.clone().into(), json_to_dynamic(v));
            }
            Dynamic::from(map)
        }
    }
}
