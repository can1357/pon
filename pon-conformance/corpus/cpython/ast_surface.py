# _ast class-hierarchy surface: category bases, per-class `_fields` /
# `_attributes` / `__match_args__` class data, PyCF_* constants, and
# keyword/positional node construction (CPython `ast_type_init` shape).
#
# ast.parse / compile(..., PyCF_ONLY_AST) is deliberately untested: pon
# compiles code objects only and refuses the AST path with a typed
# NotImplementedError, while the host oracle parses.
import ast

# Category hierarchy: leaves derive their ASDL sum type, sums derive AST.
print(issubclass(ast.Name, ast.expr), issubclass(ast.expr, ast.AST))
print(issubclass(ast.Assign, ast.stmt), issubclass(ast.Module, ast.mod))
print(issubclass(ast.Add, ast.operator), issubclass(ast.Load, ast.expr_context))
print(issubclass(ast.MatchAs, ast.pattern), issubclass(ast.TypeVar, ast.type_param))
print(issubclass(ast.ExceptHandler, ast.excepthandler), issubclass(ast.And, ast.boolop))
print(issubclass(ast.Name, ast.stmt), issubclass(ast.stmt, ast.expr))

# Class data rows: ASDL fields, location attributes, match args.
print(ast.Name._fields, ast.Name.__match_args__)
print(ast.Module._fields)
print(ast.arguments._fields)
print(ast.FunctionDef._fields)
print(ast.expr._attributes, ast.stmt._attributes)
print(ast.Name._attributes)  # inherited from ast.expr
print(ast.AST._fields, ast.AST._attributes)
print(ast.Load._fields, ast.Pass()._fields)
print(ast.Name.__module__, ast.AST.__module__)
print(type(ast.Name).__name__)

# Compiler-flag constants re-exported from _ast.
print(ast.PyCF_ONLY_AST, ast.PyCF_TYPE_COMMENTS)
print(ast.PyCF_ALLOW_TOP_LEVEL_AWAIT, ast.PyCF_OPTIMIZED_AST)

# Construction: positional args zip against _fields; keywords set attributes.
name = ast.Name("x", ast.Load())
print(name.id, type(name.ctx).__name__)
store = ast.Name(id="y", ctx=ast.Store(), lineno=3, col_offset=1)
print(store.id, type(store.ctx).__name__, store.lineno, store.col_offset)
const = ast.Constant(42, kind=None)
print(const.value, const.kind)
assign = ast.Assign([store], const, type_comment=None)
print(len(assign.targets), type(assign.value).__name__)

# Category membership of instances.
print(isinstance(name, ast.expr), isinstance(name, ast.AST), isinstance(name, ast.stmt))
print(isinstance(ast.Pass(), ast.stmt), isinstance(ast.And(), ast.boolop))

# Constructor errors: too many positionals, positional/keyword duplicates.
try:
    ast.Load(1)
except TypeError as exc:
    print("TypeError", exc)
try:
    ast.Name("x", id="y")
except TypeError as exc:
    print("TypeError", exc)

# iter_fields walks assigned fields in _fields order.
print([(field, value) for field, value in ast.iter_fields(const)])
print([field for field, value in ast.iter_fields(name)])


# NodeVisitor dispatches on the concrete class name.
class Collector(ast.NodeVisitor):
    def __init__(self):
        self.seen = []

    def visit_Name(self, node):
        self.seen.append(node.id)

    def visit_Constant(self, node):
        self.seen.append(node.value)


collector = Collector()
collector.visit(name)
collector.visit(const)
print(collector.seen)
