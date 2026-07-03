# sys.monitoring (PEP 669) constant surface: the import-time shape `bdb`
# consumes on the doctest -> pdb chain (module-level `E = sys.monitoring.events`,
# class-body dict keys and bitwise-ORs over the event flags, tool ids).
import sys

m = sys.monitoring
E = m.events
print(E.NO_EVENTS, E.PY_START, E.PY_RESUME, E.PY_RETURN, E.PY_YIELD)
print(E.CALL, E.LINE, E.INSTRUCTION, E.JUMP)
print(E.BRANCH_LEFT, E.BRANCH_RIGHT, E.STOP_ITERATION, E.RAISE)
print(E.EXCEPTION_HANDLED, E.PY_UNWIND, E.PY_THROW, E.RERAISE)
print(E.C_RETURN, E.C_RAISE, E.BRANCH)
print(m.DEBUGGER_ID, m.COVERAGE_ID, m.PROFILER_ID, m.OPTIMIZER_ID)

# bdb._MonitoringTracer class-body shapes.
EVENT_CALLBACK_MAP = {
    E.PY_START: 'call',
    E.PY_RESUME: 'call',
    E.PY_THROW: 'call',
    E.LINE: 'line',
    E.JUMP: 'jump',
    E.PY_RETURN: 'return',
    E.PY_YIELD: 'return',
    E.PY_UNWIND: 'unwind',
    E.RAISE: 'exception',
    E.STOP_ITERATION: 'exception',
    E.INSTRUCTION: 'opcode',
}
print(sorted(EVENT_CALLBACK_MAP.items()))
GLOBAL_EVENTS = E.PY_START | E.PY_RESUME | E.PY_THROW | E.PY_UNWIND | E.RAISE
LOCAL_EVENTS = E.LINE | E.JUMP | E.PY_RETURN | E.PY_YIELD | E.STOP_ITERATION
print(GLOBAL_EVENTS, LOCAL_EVENTS)
