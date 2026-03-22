use crate::parse::{Parser, AstNode};
use crate::error::KbError;

#[derive(Debug, Clone)]
pub struct TestCase {
    pub file_name: String,
    pub note: String,
    pub timeout: u32,
    pub query: Option<String>,
    pub expected_proof: Option<bool>, // true = yes, false = no
    pub expected_answer: Option<Vec<String>>, // List of expect symbol resolutions
    pub axioms: Vec<String>,
    pub extra_files: Vec<String>,
}

pub fn parse_test_content(content: &str, file_name: &str) -> Result<TestCase, KbError> {
    let (nodes, mut errors) = Parser::Kif.parse(content, file_name);
    if !errors.is_empty() {
        // Move the error out instead of cloning
        let (_, err) = errors.remove(0);
        return Err(KbError::Parse(err));
    }

    let mut tc = TestCase {
        file_name: file_name.to_string(),
        note: file_name.to_string(),
        timeout: 30,
        query: None,
        expected_answer: None,
        expected_proof: None,
        axioms: Vec::new(),
        extra_files: Vec::new(),
    };

    for node in nodes {
        let node_str = node.to_string();
        if let AstNode::List { elements, .. } = node {
            if elements.is_empty() { continue; }
            let head_name = match &elements[0] {
                AstNode::Symbol { name, .. } => name.as_str(),
                _ => {
                    tc.axioms.push(node_str.clone());
                    continue;
                }
            };

            match head_name {
                "note" => {
                    if elements.len() > 1 {
                        tc.note = match &elements[1] {
                            AstNode::Str { value, .. } => {
                                if value.starts_with('"') && value.ends_with('"') {
                                    value[1..value.len()-1].to_string()
                                } else {
                                    value.clone()
                                }
                            }
                            AstNode::Symbol { name, .. } => name.clone(),
                            _ => format!("{}", elements[1]),
                        };
                    }
                }
                "time" => {
                    if elements.len() > 1 {
                        if let AstNode::Number { value, .. } = &elements[1] {
                            tc.timeout = value.parse::<u32>().unwrap_or(30);
                        }
                    }
                }
                "answer" => {
                    if elements.len() > 1 {
                        if let AstNode::Symbol { name, .. } = &elements[1] {
                            if name.to_lowercase() == "yes" {
                                tc.expected_proof = Some(true);
                                continue;
                            } else if name.to_lowercase() == "no" {
                                tc.expected_proof = Some(false);
                                continue;
                            } 
                            
                            let mut expected_answers: Vec<String> = Vec::new();
                            tc.expected_proof = Some(true);
                            expected_answers.push(name.to_string());
                            for el in 2..elements.len() {
                                if let AstNode::Symbol { name, .. } = &elements[el] {
                                    expected_answers.push(name.to_string());
                                }
                            }
                            tc.expected_answer = Some(expected_answers);
                        }
                    }
                }
                "query" => {
                    if elements.len() > 1 {
                        tc.query = Some(elements[1].to_string());
                    }
                }
                "file" => {
                    if elements.len() > 1 {
                        let fname = match &elements[1] {
                            AstNode::Symbol { name, .. } => name.clone(),
                            AstNode::Str { value, .. } => value.trim_matches('"').to_string(),
                            _ => elements[1].to_string(),
                        };
                        tc.extra_files.push(fname);
                    }
                }
                _ => {
                    tc.axioms.push(format!("{}", node_str));
                }
            }
        }
    }

    Ok(tc)
}
