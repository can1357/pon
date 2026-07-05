#ifndef PON_COMPILE_H
#define PON_COMPILE_H

/* CPython `compile.h` compatibility shim: cython-generated C includes it for
 * the code-object flag bits (`co_flags` masks). Values mirror CPython's. */

#define CO_OPTIMIZED 0x0001
#define CO_NEWLOCALS 0x0002
#define CO_VARARGS 0x0004
#define CO_VARKEYWORDS 0x0008
#define CO_NESTED 0x0010
#define CO_GENERATOR 0x0020
#define CO_COROUTINE 0x0080
#define CO_ITERABLE_COROUTINE 0x0100
#define CO_ASYNC_GENERATOR 0x0200

#define CO_FUTURE_DIVISION 0x20000
#define CO_FUTURE_ABSOLUTE_IMPORT 0x40000
#define CO_FUTURE_WITH_STATEMENT 0x80000
#define CO_FUTURE_PRINT_FUNCTION 0x100000
#define CO_FUTURE_UNICODE_LITERALS 0x200000
#define CO_FUTURE_BARRY_AS_BDFL 0x400000
#define CO_FUTURE_GENERATOR_STOP 0x800000
#define CO_FUTURE_ANNOTATIONS 0x1000000

#endif /* PON_COMPILE_H */
