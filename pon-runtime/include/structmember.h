#ifndef PON_STRUCTMEMBER_H
#define PON_STRUCTMEMBER_H

/* CPython structmember.h compatibility: T_* member type codes and flags
 * consumed by PyMemberDef tables (the struct itself lives in Python.h). */

#include <Python.h>

#define T_SHORT 0
#define T_INT 1
#define T_LONG 2
#define T_FLOAT 3
#define T_DOUBLE 4
#define T_STRING 5
#define T_OBJECT 6
#define T_CHAR 7
#define T_BYTE 8
#define T_UBYTE 9
#define T_USHORT 10
#define T_UINT 11
#define T_ULONG 12
#define T_STRING_INPLACE 13
#define T_BOOL 14
#define T_OBJECT_EX 16
#define T_LONGLONG 17
#define T_ULONGLONG 18
#define T_PYSSIZET 19
#define T_NONE 20

#define READONLY 1
#define READ_RESTRICTED 2
#define PY_WRITE_RESTRICTED 4
#define RESTRICTED (READ_RESTRICTED | PY_WRITE_RESTRICTED)

/* Py_ prefixed aliases (CPython 3.12+). */
#define Py_T_SHORT T_SHORT
#define Py_T_INT T_INT
#define Py_T_LONG T_LONG
#define Py_T_FLOAT T_FLOAT
#define Py_T_DOUBLE T_DOUBLE
#define Py_T_STRING T_STRING
#define Py_T_OBJECT_EX T_OBJECT_EX
#define Py_T_CHAR T_CHAR
#define Py_T_BYTE T_BYTE
#define Py_T_UBYTE T_UBYTE
#define Py_T_USHORT T_USHORT
#define Py_T_UINT T_UINT
#define Py_T_ULONG T_ULONG
#define Py_T_BOOL T_BOOL
#define Py_T_LONGLONG T_LONGLONG
#define Py_T_ULONGLONG T_ULONGLONG
#define Py_T_PYSSIZET T_PYSSIZET
#define Py_T_NONE T_NONE
#define Py_READONLY READONLY

#endif /* PON_STRUCTMEMBER_H */
