// SPDX-License-Identifier: GPL-3.0-only

//! Git object id domain type.
//!
//! commit id를 `String` 그대로 다루면 branch 이름과 섞여도 compile time에 잡히지 않는다
//! (AGENTS 3.2). Git 출력에서 읽은 hex 문자열만 [`ObjectId`]로 만들 수 있게 한다.

use std::ffi::OsStr;
use std::fmt;

/// SHA-1(40자) 또는 SHA-256(64자) hex object id.
///
/// `Box<str>`로 들고 있어 `String`보다 capacity field 하나만큼 작다. plan에 후보 수만큼 쌓이므로
/// 작게 유지한다.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectId(Box<str>);

impl ObjectId {
    /// Git 출력의 hex 문자열을 검증해 [`ObjectId`]로 만든다.
    ///
    /// 길이가 40/64가 아니거나 소문자 hex가 아니면 `None`. Git은 항상 소문자로 출력하므로
    /// 대문자는 다른 출력이 섞였다는 신호로 보고 거부한다.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let valid_len = matches!(text.len(), 40 | 64);
        let valid_chars = text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        (valid_len && valid_chars).then(|| Self(text.into()))
    }

    /// hex 문자열.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ObjectId {
    /// hex 문자열 그대로 출력한다.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<OsStr> for ObjectId {
    /// Git 명령 인자로 바로 넘길 수 있게 한다.
    fn as_ref(&self) -> &OsStr {
        OsStr::new(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-1, SHA-256 길이의 소문자 hex만 받아들인다.
    #[test]
    fn parses_only_lowercase_hex_of_valid_length() {
        let sha1 = "e6ec21a92ade4f3a94039927941b1c3c3929b930";
        assert_eq!(
            ObjectId::parse(sha1).map(|id| id.to_string()),
            Some(sha1.to_owned())
        );
        assert!(ObjectId::parse(&"a".repeat(64)).is_some());
        assert!(ObjectId::parse(&sha1.to_uppercase()).is_none());
        assert!(ObjectId::parse("e6ec21a").is_none());
        assert!(ObjectId::parse("").is_none());
        assert!(ObjectId::parse(&format!("{}g", &sha1[..39])).is_none());
    }
}
