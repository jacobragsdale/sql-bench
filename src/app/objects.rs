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

use crossterm::event::{KeyCode, KeyEvent};

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
}

impl Node {
    fn new(item: Item, depth: usize) -> Self {
        Self {
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
    /// Whether the schema list itself is on its way, which is the one load
    /// with no row of its own to say so.
    loading_schemas: bool,
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
        self.loading_schemas || self.nodes.iter().any(|node| node.loading)
    }

    /// A load is on its way: the row says so until the answer arrives, and
    /// the schema list — which has no row — says so through the tree.
    pub fn started(&mut self, request: &CatalogRequest) {
        match self.find(request) {
            Some(index) => {
                self.nodes[index].loading = true;
                self.nodes[index].error = None;
            }
            None => self.loading_schemas = matches!(request, CatalogRequest::Schemas),
        }
    }

    /// What the load came back with. The results pane is the app's to fill;
    /// this is only the tree's half.
    pub fn answer(&mut self, request: &CatalogRequest, result: &Result<CatalogAnswer, DbError>) {
        self.loading_schemas = false;
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

    /// One key of the objects pane.
    pub fn key(&mut self, key: KeyEvent) -> Hit {
        if self.filtering {
            return self.filter_key(key);
        }
        #[allow(clippy::cast_possible_wrap)]
        let page = PAGE as isize;
        match key.code {
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
            KeyCode::Char('/') => {
                self.filtering = true;
                Hit::Moved
            }
            _ => Hit::Ignored,
        }
    }

    /// The keys `/` takes for itself while it is being typed into.
    fn filter_key(&mut self, key: KeyEvent) -> Hit {
        match key.code {
            KeyCode::Char(character) => self.filter.push(character),
            KeyCode::Backspace => {
                if self.filter.pop().is_none() {
                    self.filtering = false;
                }
            }
            // Esc clears it; Enter keeps it and gives the keys back.
            KeyCode::Esc => {
                self.filter.clear();
                self.filtering = false;
            }
            KeyCode::Enter => self.filtering = false,
            _ => return Hit::Ignored,
        }
        self.show_cursor();
        Hit::Moved
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
        if node.expanded {
            return self.by(1);
        }
        self.toggle()
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
        if matches!(object.kind, ObjectKind::Table | ObjectKind::Sequence) {
            return Hit::Say(format!(
                "a {} has no source text; i shows its columns",
                object.kind
            ));
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
                (Item::Object(object), CatalogRequest::Source { schema, name, .. }) => {
                    object.schema == *schema && object.name == *name
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

    /// The rows on screen, top to bottom: what is open, and — with a filter
    /// on — only the rows that match it and the rows above them.
    #[must_use]
    pub fn visible(&self) -> Vec<usize> {
        let mut open = Vec::with_capacity(self.nodes.len());
        let mut closed: Option<usize> = None;
        for (index, node) in self.nodes.iter().enumerate() {
            match closed {
                Some(depth) if node.depth > depth => continue,
                _ => closed = None,
            }
            open.push(index);
            if !node.expanded {
                closed = Some(node.depth);
            }
        }
        if self.filter.is_empty() {
            return open;
        }
        let wanted = self.filter.to_lowercase();
        let mut keep = vec![false; open.len()];
        for position in 0..open.len() {
            if !self.nodes[open[position]]
                .item
                .name()
                .to_lowercase()
                .contains(&wanted)
            {
                continue;
            }
            keep[position] = true;
            // ponytail: the ancestors are walked back one row at a time,
            // which is O(rows × depth). The tree is a screenful of rows and
            // four levels deep; an index of parents is the fix if it ever
            // holds ten thousand.
            let mut depth = self.nodes[open[position]].depth;
            for above in (0..position).rev() {
                if self.nodes[open[above]].depth < depth {
                    keep[above] = true;
                    depth = self.nodes[open[above]].depth;
                    if depth == 0 {
                        break;
                    }
                }
            }
        }
        open.into_iter()
            .zip(keep)
            .filter_map(|(index, keep)| keep.then_some(index))
            .collect()
    }

    /// The first row of a window `height` high, so the cursor is on it.
    #[must_use]
    pub fn window(&self, height: usize) -> usize {
        let height = height.max(1);
        let visible = self.visible();
        let at = visible.iter().position(|index| *index == self.cursor);
        let at = at.unwrap_or(0);
        self.scroll
            .min(at)
            .max(at.saturating_sub(height - 1))
            .min(visible.len().saturating_sub(height))
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
