# `import opcode` over the native `_opcode` seed: everything printed here is
# either pure `_opcode_metadata.py` table data or an identity over it, so pon
# and CPython agree byte for byte regardless of the interpreter's bytecode
# story. The import itself is the real exercise: opcode.py calls the seven
# `_opcode.has_*` predicates once per `opmap` value and the four table
# getters at module scope.
#
# Documented divergence (not asserted): pon has no CPython bytecode, so the
# derived category lists (`opcode.hasarg`/`hasconst`/`hasname`/`hasjump`/
# `hasfree`/`haslocal`/`hasexc`) and the `_intrinsic_*_descs`/`_nb_ops`
# tables are empty, `ENABLE_SPECIALIZATION{,_FT}` are 0, and
# `_opcode.stack_effect`/`is_valid`/`get_executor` raise NotImplementedError
# when called (see pon-runtime/src/native/opcode_.rs).
import opcode

print(opcode.cmp_op)
print(opcode.opmap["CACHE"], opcode.opname[opcode.opmap["NOP"]])
print(opcode.EXTENDED_ARG == opcode.opmap["EXTENDED_ARG"])
print(opcode.HAVE_ARGUMENT > 0, opcode.opmap["EXTENDED_ARG"] >= opcode.HAVE_ARGUMENT)
print(opcode.hascompare == [opcode.opmap["COMPARE_OP"]])
print(len(opcode.opname) == max(opcode.opmap.values()) + 1)
print(all(isinstance(name, str) and name for name in opcode.opmap))
print(opcode.hasjrel is opcode.hasjump, opcode.hasjabs)

import _opcode

# A release CPython (no Py_STATS) returns None here; so does pon.
print(_opcode.get_specialization_stats())
