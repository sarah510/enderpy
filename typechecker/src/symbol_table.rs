use bitflags::bitflags;
use rust_lapper::{Interval, Lapper};

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::{collections::HashMap, fmt::Display, path::PathBuf};

use enderpy_python_parser::ast::{self, ClassDef, FunctionDef, Node};

use crate::build::ResolvedImport;

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash)]
pub struct Id(pub u32);

#[derive(Debug, Clone)]
pub struct SymbolTable {
    pub id: Id,
    // Sub tables are scopes inside the current scope
    // after building symbol table is finished this only contains the most outer scope
    pub scopes: Vec<SymbolTableScope>,

    pub current_scope_id: u32,
    prev_scope_id: Option<u32>,

    pub file_path: PathBuf,
    // Mapping from offset to where the scope starts
    pub scope_start_offset: Lapper<u32, u32>,
    pub star_imports: Vec<Arc<ResolvedImport>>,
}

impl SymbolTable {
    pub fn new(file_path: &Path, id: Id) -> Self {
        let file_len = fs::read_to_string(file_path)
            .expect("Could not read the file")
            .len() as u32;
        let global_scope_interval = Interval {
            start: 0,
            stop: file_len,
            val: 0,
        };
        SymbolTable {
            scopes: vec![SymbolTableScope::global_scope()],
            current_scope_id: 0,
            prev_scope_id: None,
            file_path: file_path.to_path_buf(),
            scope_start_offset: Lapper::new(vec![global_scope_interval]),
            star_imports: vec![],
            id,
        }
    }

    pub fn is_pyi_file(&self) -> bool {
        self.file_path.extension().unwrap_or_default() == "pyi"
    }

    pub fn get_file_name(&self) -> String {
        return self
            .file_path
            .to_str()
            .expect("cannot get file path string")
            .split("/")
            .last()
            .expect("last part of the file")
            .to_string();
    }

    /// Do not use for lookup operations
    pub fn current_scope(&self) -> &SymbolTableScope {
        if let Some(scope) = self
            .scopes
            .iter()
            .filter(|scope| scope.id == self.current_scope_id)
            .last()
        {
            scope
        } else {
            // panic with the full string to make it easier to find the issue
            let scopes_str = self
                .scopes
                .iter()
                .map(|scope| format!("{}", scope))
                .collect::<Vec<String>>()
                .join("\n");
            panic!(
                "no current scope with id: {}. Scopes: {:?}",
                self.current_scope_id, scopes_str
            );
        }
    }

    pub fn get_parent_scope_of(&self, scope_id: u32) -> Option<&SymbolTableScope> {
        let scope = self.get_scope_by_id(scope_id).expect("scope id is invalid");
        if let Some(parent) = self.parent_scope(scope) {
            Some(parent)
        } else {
            None
        }
    }
    /// Returns the parent scope of the current scope
    pub fn parent_scope(&self, scope: &SymbolTableScope) -> Option<&SymbolTableScope> {
        let parent_id = scope.parent?;
        return Some(
            self.scopes
                .iter()
                .filter(|scope| scope.id == parent_id)
                .last()
                .expect("parent scope id not found in scopes"),
        );
    }

    pub fn parent_scope_mut(&mut self, scope: &SymbolTableScope) -> Option<&mut SymbolTableScope> {
        let parent_id = scope.parent?;
        return Some(
            self.scopes
                .iter_mut()
                .filter(|scope| scope.id == parent_id)
                .last()
                .expect("parent scope id not found in scopes"),
        );
    }

    pub fn current_scope_mut(&mut self) -> &mut SymbolTableScope {
        self.scopes
            .iter_mut()
            .filter(|scope| scope.id == self.current_scope_id)
            .last()
            .expect("no current scope")
    }

    pub fn get_scope_by_id(&self, id: u32) -> Option<&SymbolTableScope> {
        self.scopes.iter().filter(|scope| scope.id == id).last()
    }

    pub fn get_scope_mut_by_id(&mut self, id: u32) -> Option<&mut SymbolTableScope> {
        self.scopes.iter_mut().filter(|scope| scope.id == id).last()
    }

    pub fn get_enclosing_class_scope(&self) -> Option<&SymbolTableScope> {
        let mut scope = self.current_scope();
        loop {
            if let SymbolTableType::Class(_) = scope.kind {
                return Some(scope);
            }
            scope = if let Some(parent) = self.parent_scope(scope) {
                parent
            } else {
                break;
            }
        }
        None
    }

    pub fn get_enclosing_class_scope_of_scope(&self, scope: u32) -> Option<Id> {
        let mut scope = self.get_scope_by_id(scope)?;
        loop {
            if let SymbolTableType::Class(_) = scope.kind {
                return Some(Id(scope.id));
            }
            scope = if let Some(parent) = self.parent_scope(scope) {
                parent
            } else {
                break;
            }
        }
        None
    }

    pub fn current_scope_type(&self) -> &SymbolTableType {
        return &self.current_scope().kind;
    }

    /// search for symbol in that scope
    /// if not found search in parent scope continue until found or no parent scope.
    /// returns the symbol and the scope id where it was found
    pub fn lookup_in_scope(&self, name: &str, scope_id: u32) -> Option<&SymbolTableNode> {
        let mut scope = self.get_scope_by_id(scope_id).expect("no scope found");
        tracing::debug!(
            "looking for symbol {:?} in symbol table with scopes: {:?} starting from scope {}",
            name,
            self.file_path,
            scope.name,
        );
        loop {
            if let Some(symbol) = scope.symbols.get(name) {
                if (!symbol.flags.contains(SymbolFlags::INSTANCE_MEMBER)
                    && !symbol.flags.contains(SymbolFlags::CLASS_MEMBER))
                    || scope.kind.is_class()
                {
                    return Some(symbol);
                }
            }
            scope = if let Some(parent) = self.parent_scope(scope) {
                parent
            } else {
                break;
            }
        }
        None
    }

    /// Creates a new scope and sets it as the current scope
    pub fn push_scope(&mut self, new_scope: SymbolTableScope) {
        self.current_scope_id = new_scope.id;
        self.scopes.push(new_scope);
    }

    /// Sets the current scope to the scope that starts at the given position
    pub fn set_scope(&mut self, pos: u32) {
        self.prev_scope_id = Some(self.current_scope_id);
        let scope = self.scopes.iter().find(|scope| scope.start_pos == pos);
        if let Some(scope) = scope {
            self.current_scope_id = scope.id;
        } else {
            panic!("no scope found for position: {}", pos);
        }
    }

    /// Returns the id of the scope starting at position
    pub fn get_scope(&self, pos: u32) -> u32 {
        let scope = self.scopes.iter().find(|scope| scope.start_pos == pos);
        if let Some(scope) = scope {
            scope.id
        } else {
            panic!("no scope found for position: {}", pos);
        }
    }

    pub fn revert_scope(&mut self) {
        self.current_scope_id = self.prev_scope_id.expect("no previous scope");
    }

    pub fn global_scope(&self) -> &SymbolTableScope {
        self.scopes
            .iter()
            .filter(|scope| scope.id == 0)
            .last()
            .expect("no global scope")
    }

    pub fn exit_scope(&mut self) {
        let current_scope = self.current_scope();
        self.scope_start_offset.insert(Interval {
            start: current_scope.start_pos,
            stop: 0,
            val: current_scope.id,
        });
        self.current_scope_id = self
            .current_scope()
            .parent
            .expect("no parent scope. Exiting global scope is not allowed");
    }

    pub fn add_symbol(&mut self, mut symbol_node: SymbolTableNode) {
        let scope = if symbol_node.flags.contains(SymbolFlags::CLASS_MEMBER)
            || symbol_node.flags.contains(SymbolFlags::INSTANCE_MEMBER)
        {
            let mut scope = self.current_scope();
            while !matches!(scope.kind, SymbolTableType::Class(_)) {
                match self.parent_scope(scope) {
                    Some(parent) => scope = parent,
                    None => panic!("tried to assign to self outside of a class scope"),
                }
            }
            self.get_scope_mut_by_id(scope.id).expect("no scope found")
        } else {
            self.current_scope_mut()
        };

        if let Some(existing_symbol) = scope.symbols.get(&symbol_node.name) {
            symbol_node
                .declarations
                .extend(existing_symbol.declarations.clone());
        } else {
            scope.symbols.insert(symbol_node.name.clone(), symbol_node);
        }
    }

    /// Looks up an attribute in the current scope and its parents
    /// Attributes must have symbol flags CLASS_MEMBER or INSTANCE_MEMBER
    pub(crate) fn lookup_attribute(&self, attr: &str, scope_id: u32) -> Option<&SymbolTableNode> {
        let scope = self
            .get_scope_by_id(scope_id)
            .expect("scope is in the symbol table");
        return scope.symbols.get(attr);
    }
}

#[derive(Debug, Clone)]
pub struct SymbolTableScope {
    pub id: u32,
    pub start_pos: u32,
    pub kind: SymbolTableType,
    pub name: String,
    symbols: HashMap<String, SymbolTableNode>,
    parent: Option<u32>,
}

fn get_id() -> u32 {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    COUNTER.fetch_add(1, Ordering::SeqCst) as u32
}

impl SymbolTableScope {
    pub fn new(
        symbol_table_type: SymbolTableType,
        name: String,
        start_line_number: u32,
        parent: u32,
    ) -> Self {
        SymbolTableScope {
            id: get_id(),
            kind: symbol_table_type,
            name,
            symbols: HashMap::new(),
            parent: Some(parent),
            start_pos: start_line_number,
        }
    }

    pub fn global_scope() -> Self {
        SymbolTableScope {
            id: 0,
            kind: SymbolTableType::Module,
            name: String::from("global"),
            symbols: HashMap::new(),
            parent: None,
            start_pos: 0,
        }
    }
}

#[derive(Debug, Clone, is_macro::Is)]
#[allow(clippy::upper_case_acronyms)]
/// Does not include PEP 695: https://peps.python.org/pep-0695/#scoping-behavior
pub enum SymbolTableType {
    /// BUILTIN scope is used for builtins like len, print, etc.
    BUILTIN,
    Module,
    Class(Arc<ClassDef>),
    Function(Arc<FunctionDef>),
}

bitflags! {
    #[repr(transparent)]
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct SymbolFlags: u16 {
        const CLASS_MEMBER = 1 << 0;

        const INSTANCE_MEMBER = 1 << 1;

    }
}

#[derive(Debug, Clone)]
pub struct SymbolTableNode {
    pub name: String,
    pub declarations: Vec<Declaration>,
    pub flags: SymbolFlags,
}

impl Display for SymbolTableNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} - declaration: {} - properties: {:?}",
            self.name,
            self.last_declaration(),
            self.flags
        )
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DeclarationPath {
    pub symbol_table_id: Id,
    pub node: Node,
    /// The scope id that this declaration is in
    pub scope_id: u32,
}

impl DeclarationPath {
    pub fn new(symbol_table_id: Id, node: Node, scope_id: u32) -> Self {
        DeclarationPath {
            symbol_table_id,
            node,
            scope_id,
        }
    }
}

impl Display for DeclarationPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "declaration at {:?}", self.node)
    }
}

#[derive(Debug, Clone, is_macro::Is)]
pub enum Declaration {
    Variable(Variable),
    Function(Function),
    AsyncFunction(AsyncFunction),
    Class(Class),

    // Alias is used for imports
    Alias(Alias),

    Parameter(Parameter),
    // TypeParameterDeclaration represents a type parameter in a generic class or function.
    // It models type parameters declared on classes and functions like T in List[T].
    TypeParameter(TypeParameter),

    TypeAlias(TypeAlias),
}

impl Declaration {
    pub fn declaration_path(&self) -> &DeclarationPath {
        match self {
            Declaration::Variable(v) => &v.declaration_path,
            Declaration::Function(f) => &f.declaration_path,
            Declaration::AsyncFunction(f) => &f.declaration_path,
            Declaration::Class(c) => &c.declaration_path,
            Declaration::Parameter(p) => &p.declaration_path,
            Declaration::Alias(a) => &a.declaration_path,
            Declaration::TypeParameter(t) => &t.declaration_path,
            Declaration::TypeAlias(t) => &t.declaration_path,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Variable {
    pub declaration_path: DeclarationPath,
    pub type_annotation: Option<ast::Expression>,
    pub inferred_type_source: Option<ast::Expression>,
    pub is_constant: bool,
    // Determines if this variable was introduced as part of a for statement
    pub for_target: Option<ast::For>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub declaration_path: DeclarationPath,
    pub function_node: Arc<ast::FunctionDef>,
    pub is_method: bool,
    pub is_generator: bool,
    /// return statements that are reachable in the top level function body
    pub return_statements: Vec<ast::Return>,
    /// yield statements that are reachable in the top level function body
    pub yield_statements: Vec<ast::Yield>,
    /// raise statements that are reachable in the top level function body
    pub raise_statements: Vec<ast::Raise>,
}

impl Function {}

#[derive(Debug, Clone)]
pub struct AsyncFunction {
    pub declaration_path: DeclarationPath,
    pub function_node: Arc<ast::AsyncFunctionDef>,
    pub is_method: bool,
    pub is_generator: bool,
    /// return statements that are reachable in the top level function body
    pub return_statements: Vec<ast::Return>,
    /// yield statements that are reachable in the top level function body
    pub yield_statements: Vec<ast::Yield>,
    /// raise statements that are reachable in the top level function body
    pub raise_statements: Vec<ast::Raise>,
}

#[derive(Debug, Clone)]
pub struct Class {
    pub name: String,
    pub declaration_path: DeclarationPath,
    // Special classes are classes that are _SpecialForm in typeshed.
    // These classes have their behavior defined in PEPs so we need to handle them differently
    pub special: bool,
    /// Special classes have a generic class node. So this node is null for special classes
    pub class_node: Option<Arc<ClassDef>>,
    pub class_scope_id: u32,
    pub qual_name: String,
}

impl Class {
    pub fn new(
        mut module_name: String,
        class_node: Arc<ast::ClassDef>,
        declaration_path: DeclarationPath,
        class_scope_id: u32,
        // TODO: remove only to use text range here
        name: &str,
    ) -> Self {
        module_name.push('.');
        let qual_name = module_name + name;
        Class {
            name: name.to_string(),
            declaration_path,
            special: false,
            qual_name,
            class_node: Some(class_node),
            class_scope_id,
        }
    }

    /// Class node refers to SpecialForm in typeshed
    /// TODO: needs improvements mostly set the correct values
    pub fn new_special(
        name: String,
        declaration_path: DeclarationPath,
        class_scope_id: u32,
    ) -> Self {
        let qual_name = "builtins.".to_owned() + &name;
        Class {
            name,
            declaration_path,
            special: true,
            class_node: None,
            class_scope_id,
            qual_name,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Parameter {
    pub declaration_path: DeclarationPath,
    pub parameter_node: ast::Arg,
    pub type_annotation: Option<ast::Expression>,
    pub default_value: Option<ast::Expression>,
    // First parameter in methods have a special meaning. It can be self or cls.
    pub is_first: bool,
}

#[derive(Debug, Clone)]
pub struct TypeParameter {
    pub declaration_path: DeclarationPath,
    pub type_parameter_node: ast::TypeParam,
}

#[derive(Debug, Clone)]
pub struct Alias {
    pub declaration_path: DeclarationPath,
    /// The import node that this alias is for. Only one of import_node or
    /// import_from_node will be set
    pub import_from_node: Option<ast::ImportFrom>,
    pub import_node: Option<ast::Import>,
    /// Name of the imported symbol in case of ImportFrom
    /// e.g. From bar import baz -> baz is the symbol name
    pub symbol_name: Option<String>,
    /// Name of imported module in case of Import
    /// e.g. import os.path -> os.path is the module name
    pub module_name: Option<String>,
    /// The result of the import
    pub import_result: Option<Arc<ResolvedImport>>,
}

#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub declaration_path: DeclarationPath,
    pub type_alias_node: ast::TypeAlias,
}

impl SymbolTableNode {
    pub fn add_declaration(&mut self, decl: Declaration) {
        self.declarations.push(decl);
    }

    pub fn last_declaration(&self) -> &Declaration {
        self.declarations
            .last()
            .expect("There must be at least one declaration")
    }

    pub fn get_declaration_until_pos(&self, pos: u32) -> Option<&Declaration> {
        // TODO: Handle iterating declarations from last to first
        self.declarations.iter().find(|&declaration| {
            declaration.declaration_path().node.start <= pos || declaration.is_class()
        })
    }

    pub fn declaration_until_position(&self, position: u32) -> Option<&Declaration> {
        let mut filtered_declarations = self
            .declarations
            .iter()
            .filter(|decl| decl.declaration_path().node.start < position)
            .collect::<Vec<&Declaration>>();

        filtered_declarations.sort_by(|a, b| {
            a.declaration_path()
                .node
                .start
                .cmp(&b.declaration_path().node.start)
        });

        filtered_declarations.last().copied()
    }
}

// implement display for symbol table and sort the symbols by key

impl Display for SymbolTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.star_imports.is_empty() {
            writeln!(f, "Star imports:")?;
            for import in self.star_imports.iter() {
                writeln!(f, "{:?}", import.resolved_ids)?;
            }
        }
        let mut sorted_scopes = self.scopes.iter().collect::<Vec<&SymbolTableScope>>();
        sorted_scopes.sort_by(|a, b| a.name.cmp(&b.name));

        for scope in sorted_scopes.iter() {
            // Skip printing the builtin scope
            if matches!(scope.kind, SymbolTableType::BUILTIN) {
                continue;
            }
            writeln!(f, "{}", scope)?;
        }

        writeln!(f, "Scopes:\n")?;
        for scope in sorted_scopes.iter() {
            writeln!(f, "Scope {}", scope.name)?;
        }

        Ok(())
    }
}

impl Display for SymbolTableScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut sorted_symbols = self
            .symbols
            .iter()
            .collect::<Vec<(&String, &SymbolTableNode)>>();
        sorted_symbols.sort_by(|a, b| a.0.cmp(b.0));

        writeln!(f, "Symbols in {}", self.name)?;
        for (_name, symbol) in sorted_symbols {
            writeln!(f, "{}", symbol)?;
            // sort the declarations by line number
            let mut sorted_declarations = symbol.declarations.clone();
            sorted_declarations.sort_by(|a, b| {
                a.declaration_path()
                    .node
                    .start
                    .cmp(&b.declaration_path().node.start)
            });

            writeln!(f, "- Declarations:")?;

            for declaration in sorted_declarations {
                writeln!(f, "--:   {}", declaration)?;
            }
        }
        Ok(())
    }
}

impl Display for Declaration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Declaration::Variable(_v) => {
                write!(f, "Variable")
            }
            Declaration::Function(_fun) => {
                write!(f, "Function")
            }
            Declaration::Class(_c) => {
                write!(f, "Class")
            }
            Declaration::Parameter(_p) => {
                write!(f, "Parameter")
            }
            Declaration::Alias(_a) => {
                write!(f, "Alias")
            }
            Declaration::TypeParameter(_t) => {
                write!(f, "Type parameter")
            }
            Declaration::TypeAlias(_t) => {
                write!(f, "Type alias")
            }
            Declaration::AsyncFunction(_f) => {
                write!(f, "Async function")
            }
        }
    }
}
