//! Refresh schedules for refreshable materialized views.
//!
//! The forked `sqlparser` has no ClickHouse `REFRESH` grammar, so this module
//! models the schedule, renders it, and hand-parses it out of SQL text.

use serde::{Deserialize, Serialize};

/// Unit of a refresh interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum IntervalUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl IntervalUnit {
    fn as_sql(self) -> &'static str {
        match self {
            IntervalUnit::Second => "SECOND",
            IntervalUnit::Minute => "MINUTE",
            IntervalUnit::Hour => "HOUR",
            IntervalUnit::Day => "DAY",
            IntervalUnit::Week => "WEEK",
            IntervalUnit::Month => "MONTH",
            IntervalUnit::Year => "YEAR",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        let upper = word.to_ascii_uppercase();
        let singular = upper.strip_suffix('S').unwrap_or(&upper);
        Some(match singular {
            "SECOND" => IntervalUnit::Second,
            "MINUTE" => IntervalUnit::Minute,
            "HOUR" => IntervalUnit::Hour,
            "DAY" => IntervalUnit::Day,
            "WEEK" => IntervalUnit::Week,
            "MONTH" => IntervalUnit::Month,
            "YEAR" => IntervalUnit::Year,
            _ => return None,
        })
    }
}

/// A length of time such as `10 HOUR`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RefreshInterval {
    pub value: u64,
    pub unit: IntervalUnit,
}

impl RefreshInterval {
    fn sql(&self) -> String {
        format!("{} {}", self.value, self.unit.as_sql())
    }
}

/// Whether the schedule is wall-clock (`EVERY`) or relative (`AFTER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefreshKind {
    Every,
    After,
}

/// The `REFRESH …` clause of a refreshable materialized view.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshSchedule {
    pub kind: RefreshKind,
    pub interval: RefreshInterval,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<RefreshInterval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub randomize_for: Option<RefreshInterval>,
    #[serde(default)]
    pub append: bool,
    /// Reality-only: clause text this module does not model (DEPENDS ON,
    /// SETTINGS, or a clause that failed to parse). Code never sets it, so a
    /// schedule carrying it never equals a code-side one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmodelled: Option<String>,
}

impl RefreshSchedule {
    /// The clause without APPEND: `REFRESH EVERY 1 DAY OFFSET 10 HOUR`.
    pub fn schedule_sql(&self) -> String {
        let kind = match self.kind {
            RefreshKind::Every => "EVERY",
            RefreshKind::After => "AFTER",
        };
        let mut sql = format!("REFRESH {} {}", kind, self.interval.sql());
        if let Some(offset) = &self.offset {
            sql.push_str(&format!(" OFFSET {}", offset.sql()));
        }
        if let Some(randomize) = &self.randomize_for {
            sql.push_str(&format!(" RANDOMIZE FOR {}", randomize.sql()));
        }
        sql
    }

    /// `schedule_sql()` plus ` APPEND` when set: what goes between name and TO.
    pub fn to_create_clause(&self) -> String {
        let mut sql = self.schedule_sql();
        if self.append {
            sql.push_str(" APPEND");
        }
        sql
    }

    /// Placeholder for a clause that did not parse; `unmodelled = Some(raw)`.
    pub fn unparsed(raw: &str) -> Self {
        RefreshSchedule {
            kind: RefreshKind::Every,
            interval: RefreshInterval {
                value: 0,
                unit: IntervalUnit::Second,
            },
            offset: None,
            randomize_for: None,
            append: false,
            unmodelled: Some(raw.to_string()),
        }
    }

    /// True when the two differ only in kind/interval/offset/randomize_for.
    ///
    /// False when either side carries `unmodelled`, or when `append` differs
    /// (APPEND cannot be changed with `MODIFY REFRESH`).
    pub fn differs_only_in_schedule(&self, other: &Self) -> bool {
        self.unmodelled.is_none()
            && other.unmodelled.is_none()
            && self.append == other.append
            && (self.kind != other.kind
                || self.interval != other.interval
                || self.offset != other.offset
                || self.randomize_for != other.randomize_for)
    }
}

/// Why a refresh clause could not be parsed.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RefreshParseError {
    #[error("refresh clause must start with REFRESH")]
    MissingRefresh,
    #[error("expected EVERY or AFTER after REFRESH")]
    MissingKind,
    #[error("expected an interval such as `1 DAY`, found `{0}`")]
    BadInterval(String),
    #[error("unknown interval unit `{0}`")]
    UnknownUnit(String),
    #[error("OFFSET is only valid with REFRESH EVERY")]
    OffsetWithAfter,
    #[error("unexpected text after the refresh clause: `{0}`")]
    TrailingInput(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Word,
    Number,
    Quoted,
    Open,
    Close,
    Punct,
}

#[derive(Debug, Clone, Copy)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
}

fn tokenize(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'`' || c == b'\'' || c == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && c != b'`' {
                    i += 2;
                } else if bytes[i] == c {
                    // A doubled quote is an escaped quote.
                    if bytes.get(i + 1) == Some(&c) {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            i = i.min(bytes.len());
            tokens.push(Token {
                kind: TokenKind::Quoted,
                start,
                end: i,
            });
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Word,
                start,
                end: i,
            });
        } else if c.is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Number,
                start,
                end: i,
            });
        } else {
            let kind = match c {
                b'(' => TokenKind::Open,
                b')' => TokenKind::Close,
                _ => TokenKind::Punct,
            };
            // Advance one full character so multi-byte text stays on boundaries.
            i += src[i..].chars().next().map_or(1, char::len_utf8);
            tokens.push(Token {
                kind,
                start,
                end: i,
            });
        }
    }
    tokens
}

fn is_word(src: &str, token: &Token, keyword: &str) -> bool {
    token.kind == TokenKind::Word && src[token.start..token.end].eq_ignore_ascii_case(keyword)
}

fn text<'a>(src: &'a str, token: Option<&Token>) -> &'a str {
    token.map_or("", |t| &src[t.start..t.end])
}

fn parse_interval(
    src: &str,
    tokens: &[Token],
    idx: &mut usize,
) -> Result<RefreshInterval, RefreshParseError> {
    let number = tokens.get(*idx);
    let bad = || RefreshParseError::BadInterval(text(src, number).to_string());
    let value = match number {
        Some(t) if t.kind == TokenKind::Number => {
            src[t.start..t.end].parse::<u64>().map_err(|_| bad())?
        }
        _ => return Err(bad()),
    };
    let unit_token = tokens.get(*idx + 1);
    let unit = match unit_token {
        Some(t) if t.kind == TokenKind::Word => IntervalUnit::parse(text(src, Some(t)))
            .ok_or_else(|| RefreshParseError::UnknownUnit(text(src, Some(t)).to_string()))?,
        _ => return Err(bad()),
    };
    *idx += 2;
    Ok(RefreshInterval { value, unit })
}

/// Parses the modelled prefix of a clause; returns the schedule and the index
/// of the first token it did not consume.
fn parse_prefix(
    src: &str,
    tokens: &[Token],
) -> Result<(RefreshSchedule, usize), RefreshParseError> {
    if !tokens.first().is_some_and(|t| is_word(src, t, "REFRESH")) {
        return Err(RefreshParseError::MissingRefresh);
    }
    let mut idx = 1;
    let kind = match tokens.get(idx) {
        Some(t) if is_word(src, t, "EVERY") => RefreshKind::Every,
        Some(t) if is_word(src, t, "AFTER") => RefreshKind::After,
        _ => return Err(RefreshParseError::MissingKind),
    };
    idx += 1;
    let interval = parse_interval(src, tokens, &mut idx)?;
    let mut offset = None;
    if tokens.get(idx).is_some_and(|t| is_word(src, t, "OFFSET")) {
        if kind == RefreshKind::After {
            return Err(RefreshParseError::OffsetWithAfter);
        }
        idx += 1;
        offset = Some(parse_interval(src, tokens, &mut idx)?);
    }
    let mut randomize_for = None;
    if tokens
        .get(idx)
        .is_some_and(|t| is_word(src, t, "RANDOMIZE"))
        && tokens.get(idx + 1).is_some_and(|t| is_word(src, t, "FOR"))
    {
        idx += 2;
        randomize_for = Some(parse_interval(src, tokens, &mut idx)?);
    }
    let mut append = false;
    if tokens.get(idx).is_some_and(|t| is_word(src, t, "APPEND")) {
        append = true;
        idx += 1;
    }
    Ok((
        RefreshSchedule {
            kind,
            interval,
            offset,
            randomize_for,
            append,
            unmodelled: None,
        },
        idx,
    ))
}

/// Parses `REFRESH …` (with or without trailing APPEND), case-insensitive,
/// singular or plural units.
pub fn parse_refresh_clause(clause: &str) -> Result<RefreshSchedule, RefreshParseError> {
    let tokens = tokenize(clause);
    let (schedule, next) = parse_prefix(clause, &tokens)?;
    if let Some(rest) = tokens.get(next) {
        return Err(RefreshParseError::TrailingInput(
            clause[rest.start..].trim().to_string(),
        ));
    }
    Ok(schedule)
}

/// Splits a CREATE MATERIALIZED VIEW statement into the statement with the
/// refresh clause (and APPEND) removed, and the parsed schedule.
///
/// Only the region before the statement's first top-level `AS` is inspected,
/// so the SELECT body is never touched. "Top-level" means outside backtick or
/// single-quoted text and outside parentheses. A clause that does not fully
/// parse (DEPENDS ON, SETTINGS, garbage) is still removed and reported through
/// `unmodelled`. No REFRESH -> `(sql, None)`.
pub fn split_refresh_clause(create_sql: &str) -> (String, Option<RefreshSchedule>) {
    let tokens = tokenize(create_sql);

    // Header region: tokens up to the first top-level AS.
    let mut depth = 0usize;
    let mut header_end = tokens.len();
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::Open => depth += 1,
            TokenKind::Close => depth = depth.saturating_sub(1),
            TokenKind::Word if depth == 0 && is_word(create_sql, t, "AS") => {
                header_end = i;
                break;
            }
            _ => {}
        }
    }

    // First top-level REFRESH that is not the view's own name.
    let mut depth = 0usize;
    let mut refresh_idx = None;
    for (i, t) in tokens.iter().enumerate().take(header_end) {
        match t.kind {
            TokenKind::Open => depth += 1,
            TokenKind::Close => depth = depth.saturating_sub(1),
            TokenKind::Word if depth == 0 && is_word(create_sql, t, "REFRESH") => {
                let is_name = i > 0 && {
                    let prev = &tokens[i - 1];
                    is_word(create_sql, prev, "VIEW")
                        || is_word(create_sql, prev, "EXISTS")
                        || text(create_sql, Some(prev)) == "."
                };
                if !is_name {
                    refresh_idx = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(refresh_idx) = refresh_idx else {
        return (create_sql.to_string(), None);
    };

    // The clause ends at the first top-level TO or ENGINE (or the AS).
    let mut depth = 0usize;
    let mut end_idx = header_end;
    for (i, t) in tokens.iter().enumerate().take(header_end).skip(refresh_idx) {
        match t.kind {
            TokenKind::Open => depth += 1,
            TokenKind::Close => depth = depth.saturating_sub(1),
            TokenKind::Word
                if depth == 0
                    && (is_word(create_sql, t, "TO") || is_word(create_sql, t, "ENGINE")) =>
            {
                end_idx = i;
                break;
            }
            _ => {}
        }
    }

    let region_start = tokens[refresh_idx].start;
    let region_end = tokens.get(end_idx).map_or(create_sql.len(), |t| t.start);
    let raw = create_sql[region_start..region_end].trim();

    let clause_tokens = tokenize(raw);
    let schedule = match parse_prefix(raw, &clause_tokens) {
        Ok((schedule, next)) if next >= clause_tokens.len() => schedule,
        Ok((mut schedule, _)) => {
            schedule.unmodelled = Some(raw.to_string());
            schedule
        }
        Err(_) => RefreshSchedule::unparsed(raw),
    };

    let head = create_sql[..region_start].trim_end();
    let tail = &create_sql[region_end..];
    let stripped = if tail.is_empty() {
        head.to_string()
    } else {
        format!("{} {}", head, tail)
    };
    (stripped, Some(schedule))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iv(value: u64, unit: IntervalUnit) -> RefreshInterval {
        RefreshInterval { value, unit }
    }

    fn every_day() -> RefreshSchedule {
        RefreshSchedule {
            kind: RefreshKind::Every,
            interval: iv(1, IntervalUnit::Day),
            offset: None,
            randomize_for: None,
            append: false,
            unmodelled: None,
        }
    }

    #[test]
    fn renders_every_with_offset() {
        let s = RefreshSchedule {
            offset: Some(iv(10, IntervalUnit::Hour)),
            ..every_day()
        };
        assert_eq!(s.schedule_sql(), "REFRESH EVERY 1 DAY OFFSET 10 HOUR");
    }

    #[test]
    fn renders_after_randomize_append() {
        let s = RefreshSchedule {
            kind: RefreshKind::After,
            interval: iv(30, IntervalUnit::Minute),
            randomize_for: Some(iv(5, IntervalUnit::Minute)),
            append: true,
            ..every_day()
        };
        assert_eq!(
            s.to_create_clause(),
            "REFRESH AFTER 30 MINUTE RANDOMIZE FOR 5 MINUTE APPEND"
        );
    }

    #[test]
    fn parse_round_trips_every_combination() {
        for kind in [RefreshKind::Every, RefreshKind::After] {
            for offset in [None, Some(iv(10, IntervalUnit::Hour))] {
                if kind == RefreshKind::After && offset.is_some() {
                    continue;
                }
                for randomize_for in [None, Some(iv(5, IntervalUnit::Second))] {
                    for append in [false, true] {
                        let s = RefreshSchedule {
                            kind,
                            interval: iv(2, IntervalUnit::Week),
                            offset: offset.clone(),
                            randomize_for: randomize_for.clone(),
                            append,
                            unmodelled: None,
                        };
                        assert_eq!(parse_refresh_clause(&s.to_create_clause()), Ok(s));
                    }
                }
            }
        }
    }

    #[test]
    fn parses_every_unit() {
        for (word, unit) in [
            ("SECOND", IntervalUnit::Second),
            ("minutes", IntervalUnit::Minute),
            ("HOURS", IntervalUnit::Hour),
            ("Day", IntervalUnit::Day),
            ("WEEKS", IntervalUnit::Week),
            ("MONTH", IntervalUnit::Month),
            ("years", IntervalUnit::Year),
        ] {
            let s = parse_refresh_clause(&format!("REFRESH EVERY 3 {word}")).unwrap();
            assert_eq!(s.interval, iv(3, unit));
        }
    }

    #[test]
    fn parses_server_spelling() {
        let parsed = parse_refresh_clause("refresh every 1 day offset 10 hours").unwrap();
        let expected = RefreshSchedule {
            offset: Some(iv(10, IntervalUnit::Hour)),
            ..every_day()
        };
        assert_eq!(parsed, expected);
    }

    #[test]
    fn rejects_offset_with_after() {
        assert_eq!(
            parse_refresh_clause("REFRESH AFTER 1 HOUR OFFSET 5 MINUTE"),
            Err(RefreshParseError::OffsetWithAfter)
        );
    }

    #[test]
    fn rejects_bad_clauses() {
        assert_eq!(
            parse_refresh_clause("REFRESH SOMETIMES"),
            Err(RefreshParseError::MissingKind)
        );
        assert!(matches!(
            parse_refresh_clause("REFRESH EVERY 1 FORTNIGHT"),
            Err(RefreshParseError::UnknownUnit(_))
        ));
        assert!(matches!(
            parse_refresh_clause("REFRESH EVERY DAY"),
            Err(RefreshParseError::BadInterval(_))
        ));
        assert!(matches!(
            parse_refresh_clause("REFRESH EVERY 1 DAY DEPENDS ON x"),
            Err(RefreshParseError::TrailingInput(_))
        ));
        assert_eq!(
            parse_refresh_clause("EVERY 1 DAY"),
            Err(RefreshParseError::MissingRefresh)
        );
    }

    #[test]
    fn depends_on_is_unmodelled() {
        let (sql, s) = split_refresh_clause(
            "CREATE MATERIALIZED VIEW mv REFRESH EVERY 1 HOUR DEPENDS ON other TO t AS SELECT 1",
        );
        let s = s.unwrap();
        assert!(s.unmodelled.is_some());
        assert_eq!(s.interval, iv(1, IntervalUnit::Hour));
        assert_eq!(sql, "CREATE MATERIALIZED VIEW mv TO t AS SELECT 1");
    }

    #[test]
    fn split_qualified_quoted() {
        let (sql, s) = split_refresh_clause(
            "CREATE MATERIALIZED VIEW `db`.`mv` REFRESH EVERY 1 DAY OFFSET 10 HOUR TO `db`.`t` (`id` String) AS SELECT id FROM src",
        );
        assert_eq!(
            sql,
            "CREATE MATERIALIZED VIEW `db`.`mv` TO `db`.`t` (`id` String) AS SELECT id FROM src"
        );
        assert_eq!(
            s,
            Some(RefreshSchedule {
                offset: Some(iv(10, IntervalUnit::Hour)),
                ..every_day()
            })
        );
    }

    #[test]
    fn split_strips_append() {
        let (sql, s) = split_refresh_clause(
            "CREATE MATERIALIZED VIEW mv REFRESH EVERY 1 DAY APPEND TO t AS SELECT 1",
        );
        assert_eq!(sql, "CREATE MATERIALIZED VIEW mv TO t AS SELECT 1");
        assert!(s.unwrap().append);
    }

    #[test]
    fn split_ignores_select_body() {
        let src = "CREATE MATERIALIZED VIEW mv TO t AS SELECT refresh, ' TO ' AS x FROM s";
        assert_eq!(split_refresh_clause(src), (src.to_string(), None));
    }

    #[test]
    fn split_leaves_select_body_intact_when_clause_present() {
        let (sql, s) = split_refresh_clause(
            "CREATE MATERIALIZED VIEW mv REFRESH EVERY 1 DAY OFFSET 10 HOUR TO t AS SELECT refresh, ' TO ' AS x FROM s",
        );
        assert_eq!(
            sql,
            "CREATE MATERIALIZED VIEW mv TO t AS SELECT refresh, ' TO ' AS x FROM s"
        );
        assert_eq!(
            s,
            Some(RefreshSchedule {
                offset: Some(iv(10, IntervalUnit::Hour)),
                ..every_day()
            })
        );
    }

    #[test]
    fn split_ignores_as_and_to_in_column_list() {
        let (sql, s) = split_refresh_clause(
            "CREATE MATERIALIZED VIEW `db`.`mv` REFRESH EVERY 1 HOUR TO `db`.`t` (`to` String, `as` String) AS SELECT 'x' AS `to`, 'y' AS `as` FROM src",
        );
        assert_eq!(
            sql,
            "CREATE MATERIALIZED VIEW `db`.`mv` TO `db`.`t` (`to` String, `as` String) AS SELECT 'x' AS `to`, 'y' AS `as` FROM src"
        );
        assert_eq!(
            s,
            Some(RefreshSchedule {
                interval: iv(1, IntervalUnit::Hour),
                ..every_day()
            })
        );
    }

    #[test]
    fn split_ignores_unquoted_as_inside_parens() {
        let (sql, s) = split_refresh_clause(
            "CREATE MATERIALIZED VIEW mv REFRESH EVERY 1 HOUR TO t (x String DEFAULT CAST(1 AS String)) AS SELECT 1 AS x",
        );
        assert_eq!(
            sql,
            "CREATE MATERIALIZED VIEW mv TO t (x String DEFAULT CAST(1 AS String)) AS SELECT 1 AS x"
        );
        assert_eq!(
            s,
            Some(RefreshSchedule {
                interval: iv(1, IntervalUnit::Hour),
                ..every_day()
            })
        );
    }

    #[test]
    fn split_ignores_unquoted_refresh_inside_parens() {
        let src = "CREATE MATERIALIZED VIEW mv TO t (refresh String, x String DEFAULT CAST(1 AS String)) AS SELECT 1 AS x";
        assert_eq!(split_refresh_clause(src), (src.to_string(), None));
    }

    #[test]
    fn split_ignores_view_named_refresh() {
        let src = "CREATE MATERIALIZED VIEW refresh TO t AS SELECT 1";
        assert_eq!(split_refresh_clause(src), (src.to_string(), None));
    }

    #[test]
    fn split_unparseable_is_visible() {
        let (sql, s) =
            split_refresh_clause("CREATE MATERIALIZED VIEW mv REFRESH SOMETIMES TO t AS SELECT 1");
        let s = s.unwrap();
        assert_eq!(s.unmodelled.as_deref(), Some("REFRESH SOMETIMES"));
        assert_eq!(sql, "CREATE MATERIALIZED VIEW mv TO t AS SELECT 1");
    }

    #[test]
    fn differs_only_in_schedule_cases() {
        let a = every_day();
        let b = RefreshSchedule {
            interval: iv(2, IntervalUnit::Day),
            ..every_day()
        };
        assert!(a.differs_only_in_schedule(&b));
        assert!(!a.differs_only_in_schedule(&a));

        let appended = RefreshSchedule {
            append: true,
            ..b.clone()
        };
        assert!(!a.differs_only_in_schedule(&appended));

        let unmodelled = RefreshSchedule {
            unmodelled: Some("x".into()),
            ..b
        };
        assert!(!a.differs_only_in_schedule(&unmodelled));
        assert!(!unmodelled.differs_only_in_schedule(&a));
    }
}
