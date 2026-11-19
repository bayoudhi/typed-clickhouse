//! Refresh schedules for refreshable materialized views.
//!
//! The forked `sqlparser` has no ClickHouse `REFRESH` grammar, so this module
//! models the schedule, renders it, and hand-parses it out of SQL text.

use protobuf::MessageField;
use serde::{Deserialize, Serialize};

use crate::proto::infrastructure_map::{
    refresh_schedule, RefreshInterval as ProtoRefreshInterval,
    RefreshSchedule as ProtoRefreshSchedule,
};

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

    /// The unit's length in its base: seconds for SECOND..WEEK, months for
    /// MONTH and YEAR. The two bases do not convert into each other (a month
    /// has no fixed number of seconds), so this module never folds them.
    fn base_factor(self) -> (IntervalBase, u64) {
        match self {
            IntervalUnit::Second => (IntervalBase::Seconds, 1),
            IntervalUnit::Minute => (IntervalBase::Seconds, 60),
            IntervalUnit::Hour => (IntervalBase::Seconds, 3_600),
            IntervalUnit::Day => (IntervalBase::Seconds, 86_400),
            IntervalUnit::Week => (IntervalBase::Seconds, 604_800),
            IntervalUnit::Month => (IntervalBase::Months, 1),
            IntervalUnit::Year => (IntervalBase::Months, 12),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntervalBase {
    Seconds,
    Months,
}

/// A length of time such as `10 HOUR`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RefreshInterval {
    pub value: u64,
    pub unit: IntervalUnit,
}

impl RefreshInterval {
    /// The length in a unit-independent form. The server rewrites intervals
    /// (`24 HOUR` is stored as `1 DAY`, `90 SECOND` as `1 MINUTE 30 SECOND`),
    /// so two intervals mean the same schedule when this is equal.
    ///
    /// Computed in `u128`, which cannot overflow (`u64 * u64 < 2^128`): two
    /// different huge intervals never collapse onto one saturated value.
    fn canonical(&self) -> (IntervalBase, u128) {
        let (base, factor) = self.unit.base_factor();
        (base, u128::from(self.value) * u128::from(factor))
    }

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
    /// The whole raw `REFRESH …` clause (APPEND included) when it holds text
    /// this module does not model: DEPENDS ON, SETTINGS, or a clause that
    /// failed to parse. Set when a clause is read back from the server, and
    /// when a user's single-statement `SqlResource` is migrated to a
    /// structured view; the TypeScript library never sets it.
    ///
    /// When set, [`RefreshSchedule::to_create_clause`] renders this text
    /// verbatim and the modelled fields are only a best-effort reading of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmodelled: Option<String>,
}

impl IntervalUnit {
    fn from_proto_str(unit: &str) -> Option<Self> {
        Some(match unit {
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

impl RefreshInterval {
    fn to_proto(&self) -> ProtoRefreshInterval {
        ProtoRefreshInterval {
            value: self.value,
            unit: self.unit.as_sql().to_string(),
            special_fields: Default::default(),
        }
    }

    /// `None` when the stored unit is not a known one.
    fn from_proto(proto: &ProtoRefreshInterval) -> Option<Self> {
        Some(RefreshInterval {
            value: proto.value,
            unit: IntervalUnit::from_proto_str(&proto.unit)?,
        })
    }
}

impl RefreshSchedule {
    /// Convert to proto representation.
    pub fn to_proto(&self) -> ProtoRefreshSchedule {
        let interval = self.interval.to_proto();
        let kind = match self.kind {
            RefreshKind::Every => refresh_schedule::Kind::Every(interval),
            RefreshKind::After => refresh_schedule::Kind::After(interval),
        };
        ProtoRefreshSchedule {
            kind: Some(kind),
            offset: MessageField::from_option(self.offset.as_ref().map(|o| o.to_proto())),
            randomize_for: MessageField::from_option(
                self.randomize_for.as_ref().map(|o| o.to_proto()),
            ),
            append: self.append,
            unmodelled: self.unmodelled.clone(),
            special_fields: Default::default(),
        }
    }

    /// Create from proto representation.
    ///
    /// A missing kind or an unknown unit yields [`RefreshSchedule::unparsed`],
    /// so a damaged stored schedule never equals a code-side one.
    pub fn from_proto(proto: ProtoRefreshSchedule) -> Self {
        let (kind, interval) = match &proto.kind {
            Some(refresh_schedule::Kind::Every(i)) => (RefreshKind::Every, i),
            Some(refresh_schedule::Kind::After(i)) => (RefreshKind::After, i),
            None => return Self::unparsed("stored refresh schedule has no kind"),
        };
        let optional = |field: &MessageField<ProtoRefreshInterval>| match field.as_ref() {
            Some(i) => RefreshInterval::from_proto(i).map(Some),
            None => Some(None),
        };
        match (
            RefreshInterval::from_proto(interval),
            optional(&proto.offset),
            optional(&proto.randomize_for),
        ) {
            (Some(interval), Some(offset), Some(randomize_for)) => RefreshSchedule {
                kind,
                interval,
                offset,
                randomize_for,
                append: proto.append,
                unmodelled: proto.unmodelled,
            },
            _ => Self::unparsed("stored refresh schedule has an unknown interval unit"),
        }
    }

    /// The clause rendered from the modelled fields only.
    fn modelled_clause(&self) -> String {
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
        if self.append {
            sql.push_str(" APPEND");
        }
        sql
    }

    /// The whole clause, APPEND included: what goes between the view's name
    /// and `TO` in CREATE, and after `MODIFY` in `ALTER TABLE … MODIFY REFRESH`.
    ///
    /// An `unmodelled` clause is returned verbatim, so DEPENDS ON and SETTINGS
    /// survive and a clause this module cannot parse is never replaced by the
    /// placeholder schedule of [`RefreshSchedule::unparsed`].
    ///
    /// MODIFY must repeat APPEND when the view has it: the server rejects
    /// `MODIFY REFRESH` without it on an APPEND view ("Adding or removing
    /// APPEND is not supported"). MODIFY is only planned when both sides are
    /// modelled (see [`RefreshSchedule::differs_only_in_schedule`]), but the
    /// verbatim clause is also valid after MODIFY.
    pub fn to_create_clause(&self) -> String {
        match &self.unmodelled {
            Some(raw) => raw.clone(),
            None => self.modelled_clause(),
        }
    }

    /// Placeholder for a clause that did not parse; `unmodelled = Some(raw)`.
    ///
    /// The placeholder fields (`EVERY 0 SECOND`) are never rendered:
    /// [`RefreshSchedule::to_create_clause`] returns `raw` instead.
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

    /// Equality as the server sees it: intervals are compared by length, not
    /// by spelling, because the server stores `24 HOUR` as `1 DAY`.
    pub fn is_equivalent(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.append == other.append
            && self.unmodelled == other.unmodelled
            && self.same_timing(other)
    }

    fn same_timing(&self, other: &Self) -> bool {
        fn same(a: &Option<RefreshInterval>, b: &Option<RefreshInterval>) -> bool {
            match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => a.canonical() == b.canonical(),
                _ => false,
            }
        }
        self.interval.canonical() == other.interval.canonical()
            && same(&self.offset, &other.offset)
            && same(&self.randomize_for, &other.randomize_for)
    }

    /// True when the two differ only in kind/interval/offset/randomize_for.
    ///
    /// False when either side carries `unmodelled`, or when `append` differs
    /// (APPEND cannot be changed with `MODIFY REFRESH`).
    pub fn differs_only_in_schedule(&self, other: &Self) -> bool {
        self.unmodelled.is_none()
            && other.unmodelled.is_none()
            && self.append == other.append
            && (self.kind != other.kind || !self.same_timing(other))
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
    let (mut value, mut unit) = parse_interval_part(src, tokens, idx)?;
    // The server spells some intervals as several parts (`1 MINUTE 30 SECOND`).
    // Fold them into one interval in the smallest unit used.
    while let (Some(n), Some(u)) = (tokens.get(*idx), tokens.get(*idx + 1)) {
        let follows = n.kind == TokenKind::Number
            && u.kind == TokenKind::Word
            && IntervalUnit::parse(text(src, Some(u))).is_some();
        if !follows {
            break;
        }
        let (next_value, next_unit) = parse_interval_part(src, tokens, idx)?;
        let bad = || RefreshParseError::BadInterval(text(src, Some(n)).to_string());
        let (base, factor) = unit.base_factor();
        let (next_base, next_factor) = next_unit.base_factor();
        // Calendar (MONTH, YEAR) and clock units have no common base. The
        // server rejects the mix for EVERY but accepts it for AFTER
        // (`AFTER 1 MONTH 1 DAY`); either way the clause is left unmodelled
        // and rendered verbatim.
        if base != next_base {
            return Err(bad());
        }
        // u128 cannot overflow here; a total that does not fit back into u64
        // is reported, so the clause stays visible as unmodelled.
        let total = u128::from(value) * u128::from(factor)
            + u128::from(next_value) * u128::from(next_factor);
        let smallest = if next_factor < factor {
            next_unit
        } else {
            unit
        };
        value = u64::try_from(total / u128::from(smallest.base_factor().1)).map_err(|_| bad())?;
        unit = smallest;
    }
    Ok(RefreshInterval { value, unit })
}

fn parse_interval_part(
    src: &str,
    tokens: &[Token],
    idx: &mut usize,
) -> Result<(u64, IntervalUnit), RefreshParseError> {
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
    Ok((value, unit))
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

/// The `TO` target and SELECT body of a `CREATE MATERIALIZED VIEW … TO target
/// AS select` statement whose refresh clause was already removed by
/// [`split_refresh_clause`], read from the text alone.
///
/// For statements whose SELECT the SQL parser rejects. The target is the first
/// top-level `TO` that is not the view's own name, and the body is the raw text
/// after the next top-level `AS` (past any column list), trimmed and not
/// normalised. The target is returned as written, backticks included.
/// `None` when there is no target or no body.
pub(crate) fn split_materialized_view_target(create_sql: &str) -> Option<(&str, &str)> {
    let tokens = tokenize(create_sql);

    let mut depth = 0usize;
    let mut to_idx = None;
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::Open => depth += 1,
            TokenKind::Close => depth = depth.saturating_sub(1),
            TokenKind::Word if depth == 0 && is_word(create_sql, t, "AS") => return None,
            TokenKind::Word if depth == 0 && is_word(create_sql, t, "TO") => {
                let is_name = i > 0 && {
                    let prev = &tokens[i - 1];
                    is_word(create_sql, prev, "VIEW")
                        || is_word(create_sql, prev, "EXISTS")
                        || text(create_sql, Some(prev)) == "."
                };
                if !is_name {
                    to_idx = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let to_idx = to_idx?;

    // The target name: identifier parts joined by dots.
    let mut name_end = to_idx + 1;
    loop {
        match tokens.get(name_end) {
            Some(t) if matches!(t.kind, TokenKind::Word | TokenKind::Quoted) => name_end += 1,
            _ => break,
        }
        match tokens.get(name_end) {
            Some(t) if text(create_sql, Some(t)) == "." => name_end += 1,
            _ => break,
        }
    }
    if name_end == to_idx + 1 || text(create_sql, tokens.get(name_end - 1)) == "." {
        return None;
    }
    let target = &create_sql[tokens[to_idx + 1].start..tokens[name_end - 1].end];

    let mut depth = 0usize;
    for t in tokens.iter().skip(name_end) {
        match t.kind {
            TokenKind::Open => depth += 1,
            TokenKind::Close => depth = depth.saturating_sub(1),
            TokenKind::Word if depth == 0 && is_word(create_sql, t, "AS") => {
                let select = create_sql[t.end..].trim();
                return (!select.is_empty()).then_some((target, select));
            }
            _ => {}
        }
    }
    None
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
        assert_eq!(s.to_create_clause(), "REFRESH EVERY 1 DAY OFFSET 10 HOUR");
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
    fn parses_compound_intervals_the_server_writes() {
        let parsed = parse_refresh_clause(
            "REFRESH EVERY 1 DAY 12 HOUR OFFSET 1 MINUTE 30 SECOND RANDOMIZE FOR 2 HOUR",
        )
        .unwrap();
        assert_eq!(parsed.interval, iv(36, IntervalUnit::Hour));
        assert_eq!(parsed.offset, Some(iv(90, IntervalUnit::Second)));
        assert_eq!(parsed.randomize_for, Some(iv(2, IntervalUnit::Hour)));
        assert_eq!(
            parse_refresh_clause("REFRESH EVERY 1 YEAR 3 MONTH")
                .unwrap()
                .interval,
            iv(15, IntervalUnit::Month)
        );
        // Calendar and clock units are not folded. (The server rejects the
        // mix for EVERY but accepts it for AFTER; see the unmodelled tests.)
        assert!(matches!(
            parse_refresh_clause("REFRESH EVERY 1 MONTH 3 DAY"),
            Err(RefreshParseError::BadInterval(_))
        ));
    }

    #[test]
    fn equivalence_ignores_how_the_server_respells_intervals() {
        // Observed: EVERY 24 HOUR is stored as EVERY 1 DAY.
        let code = RefreshSchedule {
            interval: iv(24, IntervalUnit::Hour),
            ..every_day()
        };
        let reality = parse_refresh_clause("REFRESH EVERY 1 DAY").unwrap();
        assert_ne!(code, reality);
        assert!(code.is_equivalent(&reality));
        let offset_code = RefreshSchedule {
            offset: Some(iv(120, IntervalUnit::Minute)),
            ..every_day()
        };
        let offset_reality = parse_refresh_clause("REFRESH EVERY 1 DAY OFFSET 2 HOUR").unwrap();
        assert!(offset_code.is_equivalent(&offset_reality));
        // Differences that matter still differ.
        assert!(!code.is_equivalent(&RefreshSchedule {
            interval: iv(25, IntervalUnit::Hour),
            ..every_day()
        }));
        assert!(!code.is_equivalent(&RefreshSchedule {
            append: true,
            ..every_day()
        }));
        assert!(!offset_code.is_equivalent(&every_day()));
        // A respelling is not a schedule change, so no MODIFY REFRESH is needed.
        assert!(!code.differs_only_in_schedule(&reality));
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
    fn splits_materialized_view_target() {
        let cases: &[(&str, Option<(&str, &str)>)] = &[
            (
                "CREATE MATERIALIZED VIEW IF NOT EXISTS mv TO t AS SELECT a FROM s",
                Some(("t", "SELECT a FROM s")),
            ),
            (
                "CREATE MATERIALIZED VIEW IF NOT EXISTS `db`.`mv` TO `db`.`t` AS SELECT 'x AS y' AS a",
                Some(("`db`.`t`", "SELECT 'x AS y' AS a")),
            ),
            // The column list holds an AS of its own; the body starts after it.
            (
                "CREATE MATERIALIZED VIEW mv TO t (a String, b String ALIAS a) AS SELECT a FROM s",
                Some(("t", "SELECT a FROM s")),
            ),
            (
                "CREATE MATERIALIZED VIEW mv ON CLUSTER c TO db.t AS SELECT a FROM s",
                Some(("db.t", "SELECT a FROM s")),
            ),
            // A view named `to` is not the target keyword.
            (
                "CREATE MATERIALIZED VIEW IF NOT EXISTS to TO t AS SELECT 1",
                Some(("t", "SELECT 1")),
            ),
            (
                "CREATE MATERIALIZED VIEW mv TO t AS   SELECT 1  ",
                Some(("t", "SELECT 1")),
            ),
            ("CREATE MATERIALIZED VIEW IF NOT EXISTS mv TO t", None),
            ("CREATE MATERIALIZED VIEW mv TO t AS   ", None),
            ("CREATE MATERIALIZED VIEW mv TO AS SELECT 1", None),
            ("CREATE MATERIALIZED VIEW mv TO db. AS SELECT 1", None),
            // TO only inside the body is not a target.
            (
                "CREATE MATERIALIZED VIEW mv ENGINE = Memory AS SELECT 1 TO x",
                None,
            ),
        ];
        for (src, expected) in cases {
            assert_eq!(
                split_materialized_view_target(src),
                *expected,
                "input: {src}"
            );
        }
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
    fn unmodelled_clause_renders_verbatim() {
        let raw = "REFRESH EVERY 1 HOUR DEPENDS ON db.m SETTINGS refresh_retries = 3 APPEND";
        let (_, s) = split_refresh_clause(&format!(
            "CREATE MATERIALIZED VIEW mv {raw} TO t AS SELECT 1"
        ));
        let s = s.unwrap();
        assert_eq!(s.unmodelled.as_deref(), Some(raw));
        assert_eq!(s.to_create_clause(), raw);
    }

    #[test]
    fn unparseable_clause_never_renders_placeholder() {
        // The server accepts mixed calendar and clock units for AFTER.
        for raw in ["REFRESH AFTER 1 MONTH 1 DAY APPEND", "REFRESH SOMETIMES"] {
            let (_, s) = split_refresh_clause(&format!(
                "CREATE MATERIALIZED VIEW mv {raw} TO t AS SELECT 1"
            ));
            let rendered = s.unwrap().to_create_clause();
            assert_eq!(rendered, raw);
            assert!(!rendered.contains("EVERY 0 SECOND"));
        }
        let placeholder = RefreshSchedule::unparsed("REFRESH SOMETIMES");
        assert_eq!(placeholder.to_create_clause(), "REFRESH SOMETIMES");
    }

    #[test]
    fn overflowing_compound_interval_is_bad() {
        let clause = format!("REFRESH EVERY {} WEEK 1 SECOND", u64::MAX);
        assert!(matches!(
            parse_refresh_clause(&clause),
            Err(RefreshParseError::BadInterval(_))
        ));
        let (_, s) = split_refresh_clause(&format!(
            "CREATE MATERIALIZED VIEW mv {clause} TO t AS SELECT 1"
        ));
        assert_eq!(s.unwrap().unmodelled.as_deref(), Some(clause.as_str()));
        // A fold whose seconds overflow u64 but whose result fits is exact.
        let big = u64::MAX / 7;
        let parsed = parse_refresh_clause(&format!("REFRESH EVERY {big} WEEK 1 DAY")).unwrap();
        assert_eq!(parsed.interval, iv(big * 7 + 1, IntervalUnit::Day));
    }

    #[test]
    fn huge_intervals_do_not_saturate_into_equality() {
        let at = |value| RefreshSchedule {
            interval: iv(value, IntervalUnit::Week),
            ..every_day()
        };
        // The TypeScript library accepts up to 2^53 WEEK.
        let max_ts = 1u64 << 53;
        assert!(!at(max_ts).is_equivalent(&at(max_ts - 1)));
        assert!(!at(u64::MAX).is_equivalent(&at(u64::MAX - 1)));
        assert!(at(u64::MAX).is_equivalent(&at(u64::MAX)));
    }

    #[test]
    fn equivalence_keeps_calendar_and_clock_apart() {
        let with = |kind, value, unit| RefreshSchedule {
            kind,
            interval: iv(value, unit),
            ..every_day()
        };
        let every = RefreshKind::Every;
        assert!(!with(every, 1, IntervalUnit::Month).is_equivalent(&with(
            every,
            30,
            IntervalUnit::Day
        )));
        assert!(!with(every, 1, IntervalUnit::Month).is_equivalent(&with(
            every,
            4,
            IntervalUnit::Week
        )));
        assert!(with(every, 12, IntervalUnit::Month).is_equivalent(&with(
            every,
            1,
            IntervalUnit::Year
        )));
        assert!(!with(every, 1, IntervalUnit::Day).is_equivalent(&with(
            RefreshKind::After,
            1,
            IntervalUnit::Day
        )));
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
