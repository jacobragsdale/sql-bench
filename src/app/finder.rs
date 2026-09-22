//! `Ctrl-P`: every object of every connected tab, found as its name is
//! typed.
//!
//! The finder owns no objects. Each tab's tree keeps the [`Index`] its
//! connection answered with, and the finder keeps only the rows that
//! matched, so a keystroke is one pass over the names and the overlay draws
//! what it can show. The names are lower-cased once, when the index lands,
//! because lower-casing a hundred thousand of them on every key is the
//! difference between instant and not.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::Tab;
use super::prompt::Prompt;
use crate::db::catalog::DbObject;

/// The most rows a search keeps. A query that matches more than this wants
/// another letter, and ranking a hundred thousand hits to show twenty would
/// be paid on every keystroke.
pub const MAX_MATCHES: usize = 200;

/// How far PageUp and PageDown move. The overlay's height is not known here,
/// the way the tree's is not.
const PAGE: usize = 10;

/// Every object one connection holds, with the lower-cased names a search
/// compares against.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Index {
    objects: Vec<DbObject>,
    /// `name`, lower-cased, one per object.
    names: Vec<String>,
    /// `schema.name`, lower-cased, one per object.
    qualified: Vec<String>,
}

impl Index {
    #[must_use]
    pub fn new(objects: &[DbObject]) -> Self {
        Self {
            names: objects
                .iter()
                .map(|object| object.name.to_lowercase())
                .collect(),
            qualified: objects
                .iter()
                .map(|object| format!("{}.{}", object.schema, object.name).to_lowercase())
                .collect(),
            objects: objects.to_vec(),
        }
    }

    #[must_use]
    pub fn objects(&self) -> &[DbObject] {
        &self.objects
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

/// One row of the finder: which tab it came from, and what it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Match {
    pub tab: usize,
    pub object: DbObject,
}

/// What a key in the finder meant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Hit {
    /// The query or the cursor moved.
    Moved,
    /// Esc: the finder goes away.
    Close,
    /// Enter: go to this one.
    Open(Match),
}

/// The open finder: what is typed, what it matched, and which row is chosen.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Finder {
    pub query: Prompt,
    pub cursor: usize,
    matches: Vec<Match>,
    /// How many objects the tabs hold between them, for the title.
    indexed: usize,
    /// How many rows matched before the cap.
    hits: usize,
}

impl Finder {
    /// An empty finder over whatever the tabs have indexed so far.
    #[must_use]
    pub fn open(tabs: &[Tab]) -> Self {
        let mut finder = Self::default();
        finder.search(tabs);
        finder
    }

    #[must_use]
    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// How many objects every tab holds between them.
    #[must_use]
    pub const fn indexed(&self) -> usize {
        self.indexed
    }

    /// How many objects matched, capped or not.
    #[must_use]
    pub const fn hits(&self) -> usize {
        self.hits
    }

    /// What the overlay's title says: how much there is to search, and how
    /// much of it the query kept.
    #[must_use]
    pub fn title(&self) -> String {
        if self.indexed == 0 {
            return " Find · nothing indexed yet ".to_owned();
        }
        if self.query.text.trim().is_empty() {
            return format!(" Find · {} objects ", self.indexed);
        }
        if self.hits > MAX_MATCHES {
            return format!(
                " Find · {} of {}, first {MAX_MATCHES} ",
                self.hits, self.indexed
            );
        }
        format!(" Find · {} of {} ", self.hits, self.indexed)
    }

    /// One key. The list's keys are the tree's and fzf's — arrows, Ctrl-N
    /// and Ctrl-P, the pages — and everything else types into the query.
    pub fn key(&mut self, key: KeyEvent, tabs: &[Tab]) -> Hit {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = self.matches.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => return Hit::Close,
            KeyCode::Enter => {
                return self
                    .matches
                    .get(self.cursor)
                    .cloned()
                    .map_or(Hit::Moved, Hit::Open);
            }
            KeyCode::Down | KeyCode::Tab => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Char('n' | 'N') if control => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Up | KeyCode::BackTab => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('p' | 'P') if control => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::PageDown => self.cursor = (self.cursor + PAGE).min(last),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(PAGE),
            _ => {
                let before = self.query.text.clone();
                self.query.handle(key);
                if self.query.text != before {
                    self.search(tabs);
                }
            }
        }
        Hit::Moved
    }

    /// Match the query against every indexed tab, best first, and put the
    /// cursor back on the top row.
    ///
    /// A word with a dot in it is matched against `schema.name`, any other
    /// against the name alone; several words all have to match, and the
    /// ranks add up.
    pub fn search(&mut self, tabs: &[Tab]) {
        self.indexed = tabs
            .iter()
            .filter_map(|tab| tab.objects.index())
            .map(Index::len)
            .sum();
        self.cursor = 0;
        self.matches.clear();
        self.hits = 0;
        let query = self.query.text.to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        if words.is_empty() {
            return;
        }
        // (rank, name length, tab, position): the sort order, so a shorter
        // name wins a tie and the order is the same on every run.
        let mut ranked: Vec<(u32, usize, usize, usize)> = Vec::new();
        for (tab, index) in tabs.iter().enumerate() {
            let Some(index) = index.objects.index() else {
                continue;
            };
            for position in 0..index.len() {
                let mut total = 0;
                let mut all = true;
                for word in &words {
                    let against = if word.contains('.') {
                        &index.qualified[position]
                    } else {
                        &index.names[position]
                    };
                    match rank(word, against) {
                        Some(rank) => total = rank.saturating_add(total),
                        None => {
                            all = false;
                            break;
                        }
                    }
                }
                if all {
                    ranked.push((total, index.names[position].len(), tab, position));
                }
            }
        }
        self.hits = ranked.len();
        if ranked.len() > MAX_MATCHES {
            ranked.select_nth_unstable(MAX_MATCHES);
            ranked.truncate(MAX_MATCHES);
        }
        ranked.sort_unstable();
        self.matches = ranked
            .into_iter()
            .filter_map(|(_, _, tab, position)| {
                let object = tabs[tab].objects.index()?.objects.get(position)?.clone();
                Some(Match { tab, object })
            })
            .collect();
    }
}

/// How well `name` answers `query`, lower being better, or [`None`] for not
/// at all. The buckets are what a person expects to come first: the name
/// itself, then names starting with it, then names containing it, then
/// names its letters appear in, in order, with as little between them as
/// possible.
fn rank(query: &str, name: &str) -> Option<u32> {
    if name == query {
        return Some(0);
    }
    if name.starts_with(query) {
        return Some(1_000);
    }
    if let Some(at) = name.find(query) {
        return Some(2_000 + u32::try_from(at).unwrap_or(u32::MAX - 2_000));
    }
    subsequence(query, name).map(|gap| 3_000 + gap)
}

/// The letters of `query` in `name`, in order, taking each as early as it
/// comes: how many other bytes sit between the first and the last.
fn subsequence(query: &str, name: &str) -> Option<u32> {
    let mut rest = name.char_indices();
    let mut first = None;
    let mut end = 0;
    for wanted in query.chars() {
        let (at, found) = rest.by_ref().find(|(_, found)| *found == wanted)?;
        first.get_or_insert(at);
        end = at + found.len_utf8();
    }
    // The matched letters are the query's own, so the span holds at least
    // its bytes.
    let span = end - first?;
    Some(u32::try_from(span - query.len()).unwrap_or(u32::MAX - 3_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{key, object, two_tabs};
    use crate::db::catalog::{CatalogAnswer, CatalogRequest, ObjectKind};

    #[test]
    fn a_name_ranks_its_own_first_then_a_prefix_a_substring_and_a_subsequence() {
        assert_eq!(rank("orders", "orders"), Some(0));
        assert!(rank("orders", "orders_archive") < rank("orders", "old_orders"));
        assert!(rank("orders", "old_orders") < rank("orders", "o_r_d_e_r_s"));
        assert!(rank("ord", "customer_orders") < rank("ord", "o_r_d"));
        assert_eq!(rank("orders", "customers"), None);
        assert_eq!(subsequence("ord", "order"), Some(0));
        assert_eq!(subsequence("ors", "orders"), Some(3));
        // The last letter three bytes wide, a one-byte gap before it.
        assert_eq!(subsequence("a日", "ax日"), Some(1));
        assert_eq!(subsequence("é", "é"), Some(0));
    }

    /// Two tabs with an index each, so a search has two connections to
    /// find things in.
    fn indexed() -> Vec<Tab> {
        let mut app = two_tabs();
        let mssql = CatalogAnswer::Index(vec![
            object("dbo", "customers", ObjectKind::Table),
            object("bench", "sp_customer_orders", ObjectKind::Procedure),
            object("bench", "orders", ObjectKind::Table),
            object("bench", "fn_order_total", ObjectKind::Function),
        ]);
        let oracle = CatalogAnswer::Index(vec![
            object("BENCH", "CUSTOMERS", ObjectKind::Table),
            object("BENCH", "CUSTOMER_ORDERS", ObjectKind::Procedure),
            object("BENCH", "ORDER_PKG", ObjectKind::Package),
        ]);
        app.tabs[0]
            .objects
            .answer(&CatalogRequest::Index, &Ok(mssql));
        app.tabs[1]
            .objects
            .answer(&CatalogRequest::Index, &Ok(oracle));
        app.tabs
    }

    fn names(finder: &Finder) -> Vec<String> {
        finder
            .matches()
            .iter()
            .map(|found| {
                format!(
                    "{}:{}.{}",
                    found.tab, found.object.schema, found.object.name
                )
            })
            .collect()
    }

    #[test]
    fn typing_searches_every_tab_at_once_best_first_and_case_blind() {
        let tabs = indexed();
        let mut finder = Finder::open(&tabs);
        assert_eq!(finder.indexed(), 7);
        assert_eq!(finder.title(), " Find · 7 objects ");
        assert!(finder.matches().is_empty(), "nothing typed matches nothing");

        for character in "cust".chars() {
            finder.key(key(&character.to_string()), &tabs);
        }
        assert_eq!(
            names(&finder),
            [
                "0:dbo.customers",
                "1:BENCH.CUSTOMERS",
                "1:BENCH.CUSTOMER_ORDERS",
                "0:bench.sp_customer_orders",
            ]
        );
        assert_eq!(finder.title(), " Find · 4 of 7 ");
        assert_eq!(finder.hits(), 4);
    }

    #[test]
    fn a_dotted_word_matches_the_schema_and_several_words_all_have_to() {
        let tabs = indexed();
        let mut finder = Finder::open(&tabs);
        for character in "bench.ord".chars() {
            finder.key(key(&character.to_string()), &tabs);
        }
        assert_eq!(
            names(&finder),
            [
                "0:bench.orders",
                "1:BENCH.ORDER_PKG",
                "0:bench.fn_order_total",
                "1:BENCH.CUSTOMER_ORDERS",
                "0:bench.sp_customer_orders",
            ],
            "the two the schema starts first, then the ones its letters are in"
        );

        finder.query = Prompt::new("cust ord".to_owned());
        finder.search(&tabs);
        assert_eq!(
            names(&finder),
            ["1:BENCH.CUSTOMER_ORDERS", "0:bench.sp_customer_orders"]
        );

        finder.query = Prompt::new("zzz".to_owned());
        finder.search(&tabs);
        assert!(names(&finder).is_empty());
        assert_eq!(finder.title(), " Find · 0 of 7 ");
    }

    #[test]
    fn the_list_keys_move_the_cursor_and_enter_picks_the_row_under_it() {
        let tabs = indexed();
        let mut finder = Finder::open(&tabs);
        for character in "cust".chars() {
            finder.key(key(&character.to_string()), &tabs);
        }
        assert_eq!(finder.cursor, 0);
        assert_eq!(finder.key(key("Down"), &tabs), Hit::Moved);
        assert_eq!(finder.key(key("Ctrl-N"), &tabs), Hit::Moved);
        assert_eq!(finder.cursor, 2);
        finder.key(key("Ctrl-P"), &tabs);
        assert_eq!(finder.cursor, 1);
        finder.key(key("PageDown"), &tabs);
        assert_eq!(finder.cursor, 3, "the last row is as far as it goes");
        finder.key(key("Up"), &tabs);
        match finder.key(key("Enter"), &tabs) {
            Hit::Open(found) => {
                assert_eq!(found.tab, 1);
                assert_eq!(found.object.name, "CUSTOMER_ORDERS");
            }
            other => panic!("Enter gave {other:?}"),
        }
        // Another letter puts the cursor back on the best row.
        finder.key(key("o"), &tabs);
        assert_eq!(finder.cursor, 0);
        assert_eq!(finder.key(key("Esc"), &tabs), Hit::Close);
    }

    #[test]
    fn more_hits_than_the_cap_keeps_the_best_of_them_and_says_how_many_there_were() {
        let mut app = two_tabs();
        let mut objects: Vec<DbObject> = (0..1_000)
            .map(|n| object("dbo", &format!("table_{n:04}"), ObjectKind::Table))
            .collect();
        objects.push(object("dbo", "t", ObjectKind::Table));
        app.tabs[0]
            .objects
            .answer(&CatalogRequest::Index, &Ok(CatalogAnswer::Index(objects)));
        let mut finder = Finder::open(&app.tabs);
        finder.key(key("t"), &app.tabs);
        assert_eq!(finder.hits(), 1_001);
        assert_eq!(finder.matches().len(), MAX_MATCHES);
        assert_eq!(finder.matches()[0].object.name, "t");
        assert_eq!(finder.matches()[1].object.name, "table_0000");
        assert_eq!(finder.title(), " Find · 1001 of 1001, first 200 ");
    }
}
