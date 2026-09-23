use anyhow::{bail, Result};

pub fn find_matching_brace(s: &str, open: usize) -> Result<usize> {
    let bytes = s.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        bail!("expected '{{' at byte {open}");
    }
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    bail!("unclosed brace from byte {open}");
}

pub fn brace_content(s: &str, open: usize) -> Result<(String, usize)> {
    let close = find_matching_brace(s, open)?;
    Ok((s[open + 1..close].to_string(), close + 1))
}

pub fn optional_bracket(s: &str, start: usize) -> Result<(Option<String>, usize)> {
    if s.as_bytes().get(start) != Some(&b'[') {
        return Ok((None, start));
    }
    let mut depth = 0usize;
    for (i, &b) in s.as_bytes().iter().enumerate().skip(start) {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((Some(s[start + 1..i].to_string()), i + 1));
                }
            }
            _ => {}
        }
    }
    bail!("unclosed '[' from byte {start}");
}
