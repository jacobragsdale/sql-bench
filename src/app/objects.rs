//! One tab's object tree: schemas, the kinds under them, the objects under
//! those and a table's columns.
//!
//! The tree is a flat `Vec<Node>` with a depth on each row rather than nested
//! structs, because everything the pane does — move, expand, filter, draw the
//! window — is a walk over that list, and a borrow checker fight over a
//! recursive tree buys none of it.
//!
//! Nothing here loads anything. A node that needs children answers with the
//! [`CatalogRequest`] that would fill it and marks itself loading; the run
//! loop runs it and comes back through [`Objects::answer`].

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::chord;
use super::finder::Index;
use crate::config::Kind;
use crate::db::catalog::{CatalogAnswer, CatalogRequest, ColumnInfo, DbObject, ObjectKind};
use crate::db::model::DbError;

/// How far one level is indented.
pub const INDENT: usize = 2;

/// How far PageUp and PageDown move. The pane's height is not known here, the
/// way it is not known in the grid or the pad.
const PAGE: usize = 10;

/// What one row of the tree is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Item {
    Schema(String),
    Kind { schema: String, kind: ObjectKind },
    Object(DbObject),
    Column(ColumnInfo),
}

impl Item {
    /// What the row is drawn as.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Schema(name) => name.clone(),
            Self::Kind { kind, .. } => kind.plural().to_owned(),
            Self::Object(object) => object.name.clone(),
            Self::Column(column) => format!(
                "{} {}{}{}",
                column.name,
                column.type_text,
                if column.nullable { "" } else { " not null" },
                if column.is_pk { " pk" } else { "" },
            ),
        }
    }

    /// What `/` matches against, and what `y` copies.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Schema(name) => name.clone(),
            Self::Kind { kind, .. } => kind.plural().to_owned(),
            Self::Object(object) => object.name.clone(),
            Self::Column(column) => column.name.clone(),
        }
    }

    /// The name `y` copies: schema-qualified where there is a schema.
    #[must_use]
    pub fn qualified(&self) -> String {
        match self {
            Self::Schema(name) => name.clone(),
            Self::Kind { schema, .. } => schema.clone(),
            Self::Object(object) => format!("{}.{}", object.schema, object.name),
            Self::Column(column) => column.name.clone(),
        }
    }

    /// The object a row is, for the keys that only work on one.
    #[must_use]
    pub const fn object(&self) -> Option<&DbObject> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
        }
    }

    /// Whether a row has children worth opening.
    #[must_use]
    pub fn parent(&self) -> bool {
        match self {
            Self::Schema(_) | Self::Kind { .. } => true,
            Self::Object(object) => matches!(object.kind, ObjectKind::Table | ObjectKind::View),
            Self::Column(_) => false,
        }
    }
}

/// One row: what it is, how deep it sits and where its children are.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub item: Item,
    pub depth: usize,
    pub expanded: bool,
    /// A load is in flight for this row's children.
    pub loading: bool,
    /// Its children are here, so opening it again asks nothing.
    pub loaded: bool,
    /// What the load said instead, shown on the row.
    pub error: Option<String>,
    /// `item.name()` lower-cased once, because `/` compares against every
    /// row on every key and lower-casing fifty thousand names each time is
    /// most of what a keystroke costs.
    lower: String,
}

impl Node {
    fn new(item: Item, depth: usize) -> Self {
        Self {
            lower: item.name().to_lowercase(),
            item,
            depth,
            expanded: false,
            loading: false,
            loaded: false,
            error: None,
        }
    }
}

/// What a key in the objects pane meant. Everything with a side effect
/// leaves as one of these, so the tree itself stays pure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Hit {
    /// Not one of the tree's keys.
    Ignored,
    /// The cursor, the filter or an expanded flag moved.
    Moved,
    /// Run this catalog query; the row is already marked loading.
    Load(CatalogRequest),
    /// Enter on a table or a view: a select for the pad.
    Select(DbObject),
    /// `y`.
    Copy(String),
    /// Nothing to do, and this is why.
    Say(String),
}

/// One tab's tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Objects {
    /// Which catalog it is: the kinds a schema holds differ, and only Oracle
    /// has packages.
    backend: Kind,
    /// The schema this connection lands in, which is listed first and open.
    own: String,
    nodes: Vec<Node>,
    /// Where the cursor is, as an index into `nodes`.
    cursor: usize,
    /// What `/` narrowed the tree to, case-insensitively.
    filter: String,
    /// Whether `/` is still being typed into.
    filtering: bool,
    /// Whether the schema list itself is on its way, which — with the index
    /// — is a load with no row of its own to say so.
    loading_schemas: bool,
    /// Every object the connection holds, once the index has come back:
    /// what `Ctrl-P` searches, and what a kind's branch fills from without
    /// asking the server again.
    index: Option<Index>,
    /// The index is on its way; the pane's title says so.
    indexing: bool,
    /// Where the window starts; the pane's height is known only to the
    /// renderer, so this is a hint [`Objects::window`] clamps.
    scroll: usize,
}

impl Objects {
    /// Oracle folds the user name to upper case the way it folds everything
    /// else, so that is the schema it lands in; on SQL Server the login is
    /// not a schema and `dbo` is where it lands.
    #[must_use]
    pub fn new(backend: Kind, user: &str) -> Self {
        Self {
            backend,
            own: match backend {
                Kind::Oracle => user.to_uppercase(),
                Kind::Mssql => "dbo".to_owned(),
            },
            nodes: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filtering: false,
            loading_schemas: false,
            index: None,
            indexing: false,
            scroll: 0,
        }
    }

    /// A tree with nothing in it, which is what a disconnect leaves.
    pub fn clear(&mut self) {
        *self = Self {
            backend: self.backend,
            own: std::mem::take(&mut self.own),
            ..Self::new(self.backend, "")
        };
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    #[must_use]
    pub const fn filtering(&self) -> bool {
        self.filtering
    }

    /// Whether a catalog query this tree asked for is still running, which
    /// is what a replay's `wait busy` waits out.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.loading_schemas || self.indexing || self.nodes.iter().any(|node| node.loading)
    }

    /// Every object of the connection, once the index has come back.
    #[must_use]
    pub const fn index(&self) -> Option<&Index> {
        self.index.as_ref()
    }

    /// Whether the index is still on its way.
    #[must_use]
    pub const fn indexing(&self) -> bool {
        self.indexing
    }

    /// A load is on its way: the row says so until the answer arrives, and
    /// the schema list and the index — which have no row — say so through
    /// the tree.
    pub fn started(&mut self, request: &CatalogRequest) {
        match request {
            CatalogRequest::Schemas => self.loading_schemas = true,
            CatalogRequest::Index => self.indexing = true,
            _ => {
                if let Some(index) = self.find(request) {
                    self.nodes[index].loading = true;
                    self.nodes[index].error = None;
                }
            }
        }
    }

    /// What the load came back with. The results pane is the app's to fill;
    /// this is only the tree's half.
    pub fn answer(&mut self, request: &CatalogRequest, result: &Result<CatalogAnswer, DbError>) {
        match request {
            CatalogRequest::Schemas => self.loading_schemas = false,
            CatalogRequest::Index => self.indexing = false,
            _ => {}
        }
        let index = self.find(request);
        if let Some(index) = index {
            self.nodes[index].loading = false;
        }
        let answer = match result {
            Ok(answer) => answer,
            Err(error) => {
                if let Some(index) = index {
                    self.nodes[index].error = Some(error.to_string());
                    self.nodes[index].expanded = false;
                }
                return;
            }
        };
        match (answer, index) {
            (CatalogAnswer::Schemas(schemas), _) => self.fill_schemas(schemas),
            (CatalogAnswer::Index(objects), _) => self.fill_index(objects),
            (CatalogAnswer::Objects(objects), Some(index)) => {
                let depth = self.nodes[index].depth + 1;
                let children = objects
                    .iter()
                    .map(|object| Node::new(Item::Object(object.clone()), depth))
                    .collect();
                self.fill(index, children);
            }
            (CatalogAnswer::Columns(columns), Some(index)) => {
                let depth = self.nodes[index].depth + 1;
                let children = columns
                    .iter()
                    .map(|column| Node::new(Item::Column(column.clone()), depth))
                    .collect();
                self.fill(index, children);
            }
            // A source has no children; the pane shows it.
            (CatalogAnswer::Source(_), Some(index)) => self.nodes[index].loaded = true,
            _ => {}
        }
    }

    /// The schema list, the connection's own schema first and open: the
    /// schema somebody connected as is the one they came to look at.
    fn fill_schemas(&mut self, schemas: &[String]) {
        let mut schemas = schemas.to_vec();
        if let Some(at) = schemas.iter().position(|schema| *schema == self.own) {
            let own = schemas.remove(at);
            schemas.insert(0, own);
        }
        self.nodes = schemas
            .into_iter()
            .map(|schema| Node::new(Item::Schema(schema), 0))
            .collect();
        self.cursor = 0;
        if !self.nodes.is_empty() {
            self.open(0);
        }
    }

    /// The index landed: keep it, and refill every kind branch that was
    /// opened before it came, so a branch loaded from the server and one
    /// loaded from the index never disagree.
    fn fill_index(&mut self, objects: &[DbObject]) {
        self.index = Some(Index::new(objects));
        // A refill changes how many rows there are, so the end is read anew.
        let mut at = 0;
        while at < self.nodes.len() {
            if matches!(self.nodes[at].item, Item::Kind { .. }) && self.nodes[at].loaded {
                self.fill_kind(at);
            }
            at += 1;
        }
        // A filter typed before it came is waiting for it.
        if self.filtering || !self.filter.is_empty() {
            self.fill_all();
        }
    }

    /// A kind's objects from the index, under its row.
    fn fill_kind(&mut self, at: usize) {
        let (Item::Kind { schema, kind }, Some(index)) = (&self.nodes[at].item, &self.index) else {
            return;
        };
        let depth = self.nodes[at].depth + 1;
        let children = index
            .objects()
            .iter()
            .filter(|object| object.kind == *kind && object.schema == *schema)
            .map(|object| Node::new(Item::Object(object.clone()), depth))
            .collect();
        self.fill(at, children);
    }

    /// Put the cursor on `object`, opening the branches down to it and
    /// dropping the filter that would hide it. `false` when there is no row
    /// for it: no schema of that name, or a kind branch that would have to
    /// ask the server, which is what an object that is not in the index
    /// looks like.
    pub fn reveal(&mut self, object: &DbObject) -> bool {
        let Some(schema) = self
            .nodes
            .iter()
            .position(|node| matches!(&node.item, Item::Schema(name) if *name == object.schema))
        else {
            return false;
        };
        if !self.nodes[schema].expanded {
            self.open(schema);
        }
        let Some(kind) = (schema + 1..self.subtree_end(schema)).find(
            |at| matches!(&self.nodes[*at].item, Item::Kind { kind, .. } if *kind == object.kind),
        ) else {
            return false;
        };
        if !self.nodes[kind].expanded && self.open(kind) != Hit::Moved {
            self.nodes[kind].expanded = false;
            return false;
        }
        let Some(row) = (kind + 1..self.subtree_end(kind)).find(|at| {
            self.nodes[*at]
                .item
                .object()
                .is_some_and(|found| found.name == object.name)
        }) else {
            return false;
        };
        self.filter.clear();
        self.filtering = false;
        self.cursor = row;
        if let Some(at) = self.visible().iter().position(|index| *index == row) {
            self.scroll_to(at);
        }
        true
    }

    /// Every branch nobody has opened yet, filled from the index, so `/`
    /// finds what is under a closed branch too. What is already loaded is
    /// kept: it may have columns open under it.
    fn fill_all(&mut self) {
        let whole = self
            .nodes
            .iter()
            .all(|node| node.loaded || !matches!(node.item, Item::Schema(_) | Item::Kind { .. }));
        if whole {
            return;
        }
        let Some(index) = self.index.take() else {
            return;
        };
        let objects = index.objects();
        let mut by_kind: BTreeMap<(&str, ObjectKind), Vec<&DbObject>> = BTreeMap::new();
        for object in objects {
            by_kind
                .entry((object.schema.as_str(), object.kind))
                .or_default()
                .push(object);
        }
        let backend = self.backend;
        let push_kind = |nodes: &mut Vec<Node>, mut node: Node, schema: &str, kind| {
            node.loaded = true;
            node.error = None;
            let depth = node.depth + 1;
            nodes.push(node);
            nodes.extend(
                by_kind
                    .get(&(schema, kind))
                    .into_iter()
                    .flatten()
                    .map(|object| Node::new(Item::Object((*object).clone()), depth)),
            );
        };
        let old = std::mem::take(&mut self.nodes);
        let cursor = self.cursor;
        let mut nodes = Vec::with_capacity(old.len() + objects.len());
        for (index, mut node) in old.into_iter().enumerate() {
            if index == cursor {
                self.cursor = nodes.len();
            }
            match node.item.clone() {
                Item::Schema(schema) if !node.loaded => {
                    node.loaded = true;
                    let depth = node.depth + 1;
                    nodes.push(node);
                    for kind in ObjectKind::all_for(backend) {
                        let item = Item::Kind {
                            schema: schema.clone(),
                            kind,
                        };
                        push_kind(&mut nodes, Node::new(item, depth), &schema, kind);
                    }
                }
                Item::Kind { schema, kind } if !node.loaded => {
                    push_kind(&mut nodes, node, &schema, kind);
                }
                _ => nodes.push(node),
            }
        }
        self.nodes = nodes;
        self.index = Some(index);
        if !self.filter.is_empty() {
            self.seek(0);
        }
    }

    /// One key of the objects pane.
    pub fn key(&mut self, key: KeyEvent) -> Hit {
        if self.filtering {
            return self.filter_key(key);
        }
        #[allow(clippy::cast_possible_wrap)]
        let page = PAGE as isize;
        match key.code {
            // Ctrl-R is not `r`: a chord is some other pane's key, or none.
            KeyCode::Char(_) if chord(key) => Hit::Ignored,
            KeyCode::Char('j') | KeyCode::Down => self.by(1),
            KeyCode::Char('k') | KeyCode::Up => self.by(-1),
            KeyCode::PageDown => self.by(page),
            KeyCode::PageUp => self.by(-page),
            KeyCode::Char('g') => self.at(0),
            KeyCode::Char('G') => self.at(usize::MAX),
            KeyCode::Char('l') | KeyCode::Right => self.forward(),
            KeyCode::Char('h') | KeyCode::Left => self.back(),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Enter => self.activate(),
            KeyCode::Char('s') => self.source(),
            KeyCode::Char('i') => self.columns(),
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('y') => self.copy(),
            KeyCode::Char('/') => self.search(),
            // A filter Enter committed is still a filter, and Esc is the way
            // out of one whether or not it is being typed into.
            KeyCode::Esc if !self.filter.is_empty() => {
                self.clear_filter();
                Hit::Moved
            }
            _ => Hit::Ignored,
        }
    }

    /// `/`: start typing a filter over every object of the connection,
    /// which the index has. One still on its way fills the tree when it
    /// lands; one that failed is asked for again.
    pub fn search(&mut self) -> Hit {
        self.filtering = true;
        if self.index.is_some() {
            self.fill_all();
            return Hit::Moved;
        }
        if self.indexing || self.nodes.is_empty() {
            return Hit::Moved;
        }
        Hit::Load(CatalogRequest::Index)
    }

    /// A paste while the filter is being typed into: one more piece of it.
    pub fn paste_filter(&mut self, text: &str) {
        self.filter.push_str(text);
        self.seek(0);
    }

    /// The keys `/` takes for itself while it is being typed into.
    fn filter_key(&mut self, key: KeyEvent) -> Hit {
        match key.code {
            KeyCode::Char('u' | 'U') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.clear();
                self.seek(0);
            }
            KeyCode::Char(_) if chord(key) => return Hit::Ignored,
            KeyCode::Char(character) => {
                self.filter.push(character);
                self.seek(0);
            }
            KeyCode::Backspace => {
                if self.filter.pop().is_none() {
                    self.filtering = false;
                }
                self.seek(0);
            }
            KeyCode::Down => self.seek(1),
            KeyCode::Up => self.seek(-1),
            // Esc clears it; Enter keeps it and gives the keys back.
            KeyCode::Esc => self.clear_filter(),
            KeyCode::Enter => self.filtering = false,
            _ => return Hit::Ignored,
        }
        Hit::Moved
    }

    /// Put the cursor on the next row the filter matches (`by` 1), the one
    /// before (-1) or the first (0), so what was typed is what Enter opens.
    fn seek(&mut self, by: isize) {
        if self.filter.is_empty() {
            self.show_cursor();
            return;
        }
        let visible = self.visible();
        let wanted = self.filter.to_lowercase();
        let hit = |index: &usize| matches(&self.nodes[*index], &wanted);
        let at = visible.iter().position(|index| *index == self.cursor);
        let found = match (by, at) {
            (1, Some(at)) => visible[at + 1..]
                .iter()
                .position(hit)
                .map(|found| at + 1 + found),
            (-1, Some(at)) => visible[..at].iter().rposition(hit),
            _ => visible.iter().position(hit),
        };
        match found {
            Some(found) => {
                self.cursor = visible[found];
                self.scroll_to(found);
            }
            None => self.show_cursor(),
        }
    }

    /// Drop the filter and open every branch above the cursor, so the row it
    /// found is still the one under it.
    fn clear_filter(&mut self) {
        self.filter.clear();
        self.filtering = false;
        let mut depth = self.nodes.get(self.cursor).map_or(0, |node| node.depth);
        for index in (0..self.cursor).rev() {
            if depth == 0 {
                break;
            }
            if self.nodes[index].depth < depth {
                self.nodes[index].expanded = true;
                depth = self.nodes[index].depth;
            }
        }
        self.show_cursor();
    }

    /// `l`: open what is closed, step into what is open, and show what has
    /// nothing under it.
    fn forward(&mut self) -> Hit {
        let Some(node) = self.nodes.get(self.cursor) else {
            return Hit::Ignored;
        };
        if !node.item.parent() {
            return self.activate();
        }
        if node.expanded || self.shows_children(self.cursor) {
            return self.by(1);
        }
        self.toggle()
    }

    /// Whether the row after `index` on screen is under it, which a filter
    /// does for a closed branch with a match in it.
    fn shows_children(&self, index: usize) -> bool {
        let visible = self.visible();
        visible
            .iter()
            .position(|row| *row == index)
            .and_then(|at| visible.get(at + 1))
            .is_some_and(|next| self.nodes[*next].depth > self.nodes[index].depth)
    }

    /// `h`: close what is open, and go up a level from what is closed.
    fn back(&mut self) -> Hit {
        let Some(node) = self.nodes.get(self.cursor) else {
            return Hit::Ignored;
        };
        if node.expanded {
            self.nodes[self.cursor].expanded = false;
            return Hit::Moved;
        }
        let depth = node.depth;
        let parent = self
            .visible()
            .into_iter()
            .rev()
            .find(|index| *index < self.cursor && self.nodes[*index].depth < depth);
        if let Some(parent) = parent {
            self.cursor = parent;
        }
        Hit::Moved
    }

    /// Space: open a closed row, close an open one.
    fn toggle(&mut self) -> Hit {
        let cursor = self.cursor;
        let Some(node) = self.nodes.get_mut(cursor) else {
            return Hit::Ignored;
        };
        if !node.item.parent() {
            return Hit::Ignored;
        }
        if node.expanded {
            node.expanded = false;
            return Hit::Moved;
        }
        self.open(cursor)
    }

    /// Open a row, and say what has to be loaded for it to have children.
    fn open(&mut self, index: usize) -> Hit {
        let Some(node) = self.nodes.get_mut(index) else {
            return Hit::Ignored;
        };
        node.expanded = true;
        if node.loaded {
            return Hit::Moved;
        }
        // A schema's children are the kinds themselves, which no server has
        // to be asked about.
        if let Item::Schema(schema) = node.item.clone() {
            let backend = self.backend;
            let depth = self.nodes[index].depth + 1;
            let children = ObjectKind::all_for(backend)
                .into_iter()
                .map(|kind| {
                    Node::new(
                        Item::Kind {
                            schema: schema.clone(),
                            kind,
                        },
                        depth,
                    )
                })
                .collect();
            self.fill(index, children);
            return Hit::Moved;
        }
        // A kind's objects are in the index once it is here, so the server
        // is only asked before that.
        if self.index.is_some() && matches!(self.nodes[index].item, Item::Kind { .. }) {
            self.fill_kind(index);
            return Hit::Moved;
        }
        match self.request(index) {
            Some(request) => Hit::Load(request),
            None => Hit::Moved,
        }
    }

    /// Enter: a table or a view goes into the pad, anything with source text
    /// goes into the results pane, and a branch opens.
    fn activate(&mut self) -> Hit {
        let Some(node) = self.nodes.get(self.cursor) else {
            return Hit::Ignored;
        };
        match &node.item {
            Item::Object(object) => match object.kind {
                ObjectKind::Table | ObjectKind::View => Hit::Select(object.clone()),
                _ => self.source(),
            },
            Item::Schema(_) | Item::Kind { .. } => self.toggle(),
            Item::Column(_) => Hit::Ignored,
        }
    }

    /// `s`: the text that made the object, in the results pane.
    fn source(&mut self) -> Hit {
        let Some(object) = self
            .nodes
            .get(self.cursor)
            .and_then(|node| node.item.object())
        else {
            return Hit::Say("no object here".to_owned());
        };
        if object.kind == ObjectKind::Sequence {
            return Hit::Say("a sequence has no source text".to_owned());
        }
        Hit::Load(CatalogRequest::Source {
            schema: object.schema.clone(),
            name: object.name.clone(),
            kind: object.kind,
        })
    }

    /// `i`: the columns, in the results pane as well as under the row.
    fn columns(&mut self) -> Hit {
        let Some(object) = self
            .nodes
            .get(self.cursor)
            .and_then(|node| node.item.object())
        else {
            return Hit::Say("no object here".to_owned());
        };
        if !matches!(object.kind, ObjectKind::Table | ObjectKind::View) {
            return Hit::Say(format!("a {} has no columns", object.kind));
        }
        Hit::Load(CatalogRequest::Columns {
            schema: object.schema.clone(),
            table: object.name.clone(),
            show: true,
        })
    }

    /// `r`: ask again for this row's children.
    fn reload(&mut self) -> Hit {
        let cursor = self.cursor;
        let Some(node) = self.nodes.get_mut(cursor) else {
            return Hit::Ignored;
        };
        node.loaded = false;
        node.error = None;
        // A schema's children cost nothing to rebuild, so it reloads by
        // being opened again with everything under it forgotten.
        if matches!(node.item, Item::Schema(_)) {
            self.fill(cursor, Vec::new());
            self.nodes[cursor].loaded = false;
            return self.open(cursor);
        }
        // A kind branch came from the index, so it is the index that is
        // asked for again; the branch refills when it lands.
        if self.index.is_some() && matches!(node.item, Item::Kind { .. }) {
            self.fill(cursor, Vec::new());
            self.nodes[cursor].expanded = true;
            return Hit::Load(CatalogRequest::Index);
        }
        match self.request(cursor) {
            Some(request) => {
                self.fill(cursor, Vec::new());
                self.nodes[cursor].loaded = false;
                self.nodes[cursor].expanded = true;
                Hit::Load(request)
            }
            None => Hit::Say("nothing to reload here".to_owned()),
        }
    }

    fn copy(&mut self) -> Hit {
        self.nodes
            .get(self.cursor)
            .map_or(Hit::Ignored, |node| Hit::Copy(node.item.qualified()))
    }

    /// The query that fills this row's children.
    fn request(&self, index: usize) -> Option<CatalogRequest> {
        match &self.nodes.get(index)?.item {
            Item::Kind { schema, kind } => Some(CatalogRequest::Objects {
                schema: schema.clone(),
                kind: *kind,
            }),
            Item::Object(object) if matches!(object.kind, ObjectKind::Table | ObjectKind::View) => {
                Some(CatalogRequest::Columns {
                    schema: object.schema.clone(),
                    table: object.name.clone(),
                    show: false,
                })
            }
            _ => None,
        }
    }

    /// The row a request is about, or [`None`] for the schema list, which is
    /// the whole tree rather than one row of it.
    fn find(&self, request: &CatalogRequest) -> Option<usize> {
        self.nodes
            .iter()
            .position(|node| match (&node.item, request) {
                (
                    Item::Kind { schema, kind },
                    CatalogRequest::Objects {
                        schema: want,
                        kind: which,
                    },
                ) => schema == want && kind == which,
                (Item::Object(object), CatalogRequest::Columns { schema, table, .. }) => {
                    object.schema == *schema && object.name == *table
                }
                // Kind too: one name can be two objects of different kinds.
                (Item::Object(object), CatalogRequest::Source { schema, name, kind }) => {
                    object.schema == *schema && object.name == *name && object.kind == *kind
                }
                _ => false,
            })
    }

    /// Put `children` under `index`, in place of whatever was there.
    fn fill(&mut self, index: usize, children: Vec<Node>) {
        let end = self.subtree_end(index);
        let had = end - index - 1;
        #[allow(clippy::cast_possible_wrap)]
        let delta = children.len() as isize - had as isize;
        self.nodes.splice(index + 1..end, children);
        self.nodes[index].loaded = true;
        // The cursor is an index, so rows appearing above it move it.
        if self.cursor >= end {
            self.cursor = self.cursor.saturating_add_signed(delta);
        } else if self.cursor > index {
            self.cursor = index;
        }
        self.show_cursor();
    }

    /// One past the last row under `index`.
    fn subtree_end(&self, index: usize) -> usize {
        let depth = self.nodes[index].depth;
        let mut end = index + 1;
        while end < self.nodes.len() && self.nodes[end].depth > depth {
            end += 1;
        }
        end
    }

    /// The rows on screen, top to bottom: what is open, or — with a filter
    /// on — the rows that match it, open or not, and the branches above them.
    /// A column only counts under a table that is open: `id` is in every one.
    #[must_use]
    pub fn visible(&self) -> Vec<usize> {
        let mut open = vec![false; self.nodes.len()];
        let mut closed: Option<usize> = None;
        for (index, node) in self.nodes.iter().enumerate() {
            match closed {
                Some(depth) if node.depth > depth => continue,
                _ => closed = None,
            }
            open[index] = true;
            if !node.expanded {
                closed = Some(node.depth);
            }
        }
        if self.filter.is_empty() {
            return (0..self.nodes.len()).filter(|index| open[*index]).collect();
        }
        let wanted = self.filter.to_lowercase();
        let mut keep = vec![false; self.nodes.len()];
        // The rows above this one, nearest last.
        let mut path: Vec<usize> = Vec::new();
        for (index, node) in self.nodes.iter().enumerate() {
            while path
                .last()
                .is_some_and(|above| self.nodes[*above].depth >= node.depth)
            {
                path.pop();
            }
            let counts = open[index] || !matches!(node.item, Item::Column(_));
            if counts && matches(node, &wanted) {
                keep[index] = true;
                for above in path.iter().rev() {
                    if std::mem::replace(&mut keep[*above], true) {
                        break;
                    }
                }
            }
            path.push(index);
        }
        (0..self.nodes.len()).filter(|index| keep[*index]).collect()
    }

    /// The first row of a window `height` high over the [`Self::visible`]
    /// rows, so the cursor is on it. The rows are the caller's, who drew
    /// them, because working them out is a walk of the whole tree.
    #[must_use]
    pub fn window(&self, visible: &[usize], height: usize) -> usize {
        let height = height.max(1);
        let at = visible.iter().position(|index| *index == self.cursor);
        let at = at.unwrap_or(0);
        self.scroll
            .min(at)
            .max(at.saturating_sub(height - 1))
            .min(visible.len().saturating_sub(height))
    }

    /// A click on the `row`th row of a window drawn from `top`: the cursor
    /// goes there and the window stays drawn from `top`, which is set here
    /// and not worked out by `scroll_to`, whose page rule would move a view
    /// whose bottom row was clicked. Whether there was a row there.
    ///
    /// ponytail: the first `j` after a click ten or more rows below `top`
    /// still moves the view once, by `scroll_to`'s page rule. A real page
    /// height in the app would end that.
    pub fn click(&mut self, top: usize, row: usize) -> bool {
        let Some(index) = self.visible().get(top + row).copied() else {
            return false;
        };
        self.cursor = index;
        self.scroll = top;
        true
    }

    /// Whether `column` of the cursor's row is its `▸` or `▾`.
    #[must_use]
    pub fn on_glyph(&self, column: usize) -> bool {
        self.nodes.get(self.cursor).is_some_and(|node| {
            let at = node.depth * INDENT;
            node.item.parent() && (at..at + 2).contains(&column)
        })
    }

    /// The wheel over a window `height` rows high drawn from `top`: the
    /// window moves by `by` and the cursor comes along only as far as it
    /// has to, to stay on it.
    ///
    /// ponytail: the cursor is pulled along because the window is a hint
    /// clamped round it; scrolling it off the screen needs the pane's height
    /// in the app.
    pub fn wheel(&mut self, top: usize, by: isize, height: usize) {
        let visible = self.visible();
        let top = top
            .saturating_add_signed(by)
            .min(visible.len().saturating_sub(height));
        let at = visible
            .iter()
            .position(|index| *index == self.cursor)
            .unwrap_or(0)
            .clamp(top, top + height.max(1) - 1);
        if let Some(index) = visible.get(at) {
            self.cursor = *index;
        }
        self.scroll = top;
    }

    fn by(&mut self, delta: isize) -> Hit {
        let visible = self.visible();
        if visible.is_empty() {
            return Hit::Moved;
        }
        let at = visible
            .iter()
            .position(|index| *index == self.cursor)
            .unwrap_or(0);
        let wanted = at.saturating_add_signed(delta).min(visible.len() - 1);
        self.cursor = visible[wanted];
        self.scroll_to(wanted);
        Hit::Moved
    }

    fn at(&mut self, row: usize) -> Hit {
        let visible = self.visible();
        if visible.is_empty() {
            return Hit::Moved;
        }
        let wanted = row.min(visible.len() - 1);
        self.cursor = visible[wanted];
        self.scroll_to(wanted);
        Hit::Moved
    }

    /// Put the cursor back on a row that is showing, which a filter or a
    /// collapse can take away from it.
    fn show_cursor(&mut self) {
        let visible = self.visible();
        if visible.contains(&self.cursor) {
            return;
        }
        self.cursor = visible
            .iter()
            .copied()
            .take_while(|index| *index < self.cursor)
            .last()
            .or_else(|| visible.first().copied())
            .unwrap_or(0);
    }

    fn scroll_to(&mut self, at: usize) {
        if at < self.scroll {
            self.scroll = at;
        } else if at >= self.scroll + PAGE {
            self.scroll = at + 1 - PAGE;
        }
    }
}

/// Whether a row is one the filter `wanted` (lower case) is looking for: its
/// name has it in it, or with a dot in it, an object's `schema.name` does.
/// A branch of kinds is never one: `t` would keep every `Tables`.
fn matches(node: &Node, wanted: &str) -> bool {
    match &node.item {
        Item::Kind { .. } => false,
        Item::Object(object) if wanted.contains('.') => {
            format!("{}.{}", object.schema, object.name)
                .to_lowercase()
                .contains(wanted)
        }
        _ => node.lower.contains(wanted),
    }
}
