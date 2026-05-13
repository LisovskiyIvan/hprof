use std::collections::HashMap;
use std::fs;

use crate::types::*;

pub struct HeapProfile {
    file_path: String,
    data: Option<HeapProfileResult>,
}

impl HeapProfile {
    pub fn new(file_path: String) -> Self {
        Self { file_path, data: None }
    }

    pub fn data(&mut self) -> crate::Result<&HeapProfileResult> {
        if self.data.is_none() {
            let raw: serde_json::Value = serde_json::from_reader(fs::File::open(&self.file_path)?)?;
            self.data = Some(HeapProfileResult {
                head: convert_node(&raw["head"]),
                start_time: raw["startTime"].as_f64().unwrap_or(0.0),
                end_time: raw["endTime"].as_f64().unwrap_or(0.0),
            });
        }
        Ok(self.data.as_ref().unwrap())
    }

    pub fn summarize(&mut self, top: Option<usize>, filter: Option<&str>) -> crate::Result<HeapProfileSummary> {
        let _ = self.data()?;
        let data = self.data.as_ref().unwrap();
        let top = top.unwrap_or(usize::MAX);

        let mut by_frame: HashMap<String, usize> = HashMap::new();
        let mut by_url: HashMap<String, usize> = HashMap::new();
        let mut by_function: HashMap<String, usize> = HashMap::new();
        let mut total_size = 0usize;

        let filter_re = filter.and_then(|f| regex::Regex::new(f).ok());

        fn walk(
            node: &HeapProfileNode,
            stack: &[String],
            filter_re: Option<&regex::Regex>,
            by_frame: &mut HashMap<String, usize>,
            by_url: &mut HashMap<String, usize>,
            by_function: &mut HashMap<String, usize>,
            total_size: &mut usize,
        ) {
            let fn_name = if node.call_frame.function_name.is_empty() {
                "(anonymous)".to_string()
            } else {
                node.call_frame.function_name.clone()
            };
            let url = if node.call_frame.url.is_empty() { "<no-url>".to_string() } else { node.call_frame.url.clone() };
            let line = node.call_frame.line_number + 1;
            let frame = format!("{} @ {}:{}", fn_name, url, line);
            let mut next_stack = stack.to_vec();
            next_stack.push(frame.clone());

            let self_size = node.self_size;
            if self_size > 0 {
                let hay = format!("{}\n{}", frame, next_stack.join("\n"));
                if let Some(re) = filter_re {
                    if !re.is_match(&hay) {
                        for child in &node.children {
                            walk(child, &next_stack, Some(re), by_frame, by_url, by_function, total_size);
                        }
                        return;
                    }
                }
                *total_size += self_size;
                *by_frame.entry(frame.clone()).or_insert(0) += self_size;
                *by_url.entry(url.clone()).or_insert(0) += self_size;
                *by_function.entry(fn_name.clone()).or_insert(0) += self_size;
            }

            for child in &node.children {
                walk(child, &next_stack, filter_re, by_frame, by_url, by_function, total_size);
            }
        }

        walk(&data.head, &[], filter_re.as_ref(), &mut by_frame, &mut by_url, &mut by_function, &mut total_size);

        let trim = |map: HashMap<String, usize>| -> HashMap<String, usize> {
            if top == usize::MAX { return map; }
            let mut v: Vec<_> = map.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            v.truncate(top);
            v.into_iter().collect()
        };

        Ok(HeapProfileSummary {
            total_size,
            by_frame: trim(by_frame),
            by_url: trim(by_url),
            by_function: trim(by_function),
        })
    }

    pub fn flatten(&mut self) -> crate::Result<Vec<FlatCallFrame>> {
        let _ = self.data()?;
        let data = self.data.as_ref().unwrap();
        let mut result = Vec::new();

        fn walk(node: &HeapProfileNode, stack: &[String], result: &mut Vec<FlatCallFrame>) {
            let fn_name = if node.call_frame.function_name.is_empty() {
                "(anonymous)".to_string()
            } else {
                node.call_frame.function_name.clone()
            };
            let url = if node.call_frame.url.is_empty() { "<no-url>".to_string() } else { node.call_frame.url.clone() };
            let frame = format!("{} @ {}:{}", fn_name, url, node.call_frame.line_number + 1);
            let mut next_stack = stack.to_vec();
            next_stack.push(frame);

            if node.self_size > 0 {
                result.push(FlatCallFrame {
                    function_name: fn_name,
                    url,
                    line_number: node.call_frame.line_number,
                    column_number: node.call_frame.column_number,
                    self_size: node.self_size,
                    stack: next_stack.clone(),
                });
            }

            for child in &node.children {
                walk(child, &next_stack, result);
            }
        }

        walk(&data.head, &[], &mut result);
        Ok(result)
    }
}

fn convert_node(val: &serde_json::Value) -> HeapProfileNode {
    let frame = &val["callFrame"];
    HeapProfileNode {
        call_frame: CallFrame {
            function_name: frame["functionName"].as_str().unwrap_or("").to_string(),
            script_id: frame["scriptId"].as_str().unwrap_or("").to_string(),
            url: frame["url"].as_str().unwrap_or("").to_string(),
            line_number: frame["lineNumber"].as_i64().unwrap_or(0) as i32,
            column_number: frame["columnNumber"].as_i64().unwrap_or(0) as i32,
        },
        self_size: val["selfSize"].as_u64().unwrap_or(0) as usize,
        children: val["children"].as_array().map(|arr| arr.iter().map(convert_node).collect()).unwrap_or_default(),
    }
}
