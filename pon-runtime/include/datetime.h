#ifndef PON_DATETIME_H
#define PON_DATETIME_H

#include <Python.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Pon datetime objects are Python-level instances from the vendored datetime
 * module. They do NOT use CPython's packed datetime/date/time/timedelta C
 * layouts. These structs exist only so source casts compile; every public
 * accessor below calls back into the runtime and reads Python attributes.
 */
typedef struct { PyObject ob_base; } PyDateTime_Delta;
typedef struct { PyObject ob_base; } PyDateTime_TZInfo;
typedef struct { PyObject ob_base; } PyDateTime_Date;
typedef struct { PyObject ob_base; } PyDateTime_DateTime;
typedef struct { PyObject ob_base; } PyDateTime_Time;

typedef struct {
    PyTypeObject *DateType;
    PyTypeObject *DateTimeType;
    PyTypeObject *TimeType;
    PyTypeObject *DeltaType;
    PyTypeObject *TZInfoType;

    PyObject *TimeZone_UTC;

    PyObject *(*Date_FromDate)(int, int, int, PyTypeObject *);
    PyObject *(*DateTime_FromDateAndTime)(int, int, int, int, int, int, int,
        PyObject *, PyTypeObject *);
    PyObject *(*Time_FromTime)(int, int, int, int, PyObject *, PyTypeObject *);
    PyObject *(*Delta_FromDelta)(int, int, int, int, PyTypeObject *);
    PyObject *(*TimeZone_FromTimeZone)(PyObject *offset, PyObject *name);

    PyObject *(*DateTime_FromTimestamp)(PyObject *, PyObject *, PyObject *);
    PyObject *(*Date_FromTimestamp)(PyObject *, PyObject *);

    PyObject *(*DateTime_FromDateAndTimeAndFold)(int, int, int, int, int, int, int,
        PyObject *, int, PyTypeObject *);
    PyObject *(*Time_FromTimeAndFold)(int, int, int, int, PyObject *, int, PyTypeObject *);
} PyDateTime_CAPI;

#define PyDateTime_CAPSULE_NAME "datetime.datetime_CAPI"

#ifndef _PY_DATETIME_IMPL
static PyDateTime_CAPI *PyDateTimeAPI = NULL;

#define PyDateTime_IMPORT \
    (PyDateTimeAPI = (PyDateTime_CAPI *)_PyPon_DateTime_CAPIImport())

#define PyDateTime_TimeZone_UTC PyDateTimeAPI->TimeZone_UTC

/* CPython oracle (python3.14): datetime is a date subclass, but date is not a
 * datetime. Use the twin-aware isinstance path; never inspect Pon object
 * headers or CPython datetime layout flags.
 */
static inline int _PyPon_DateTime_IsInstance(PyObject *op, PyTypeObject *type) {
    return PyObject_IsInstance(op, (PyObject *)type);
}

#define PyDate_Check(op) _PyPon_DateTime_IsInstance((PyObject *)(op), PyDateTimeAPI->DateType)
#define PyDate_CheckExact(op) Py_IS_TYPE((PyObject *)(op), PyDateTimeAPI->DateType)

#define PyDateTime_Check(op) _PyPon_DateTime_IsInstance((PyObject *)(op), PyDateTimeAPI->DateTimeType)
#define PyDateTime_CheckExact(op) Py_IS_TYPE((PyObject *)(op), PyDateTimeAPI->DateTimeType)

#define PyTime_Check(op) _PyPon_DateTime_IsInstance((PyObject *)(op), PyDateTimeAPI->TimeType)
#define PyTime_CheckExact(op) Py_IS_TYPE((PyObject *)(op), PyDateTimeAPI->TimeType)

#define PyDelta_Check(op) _PyPon_DateTime_IsInstance((PyObject *)(op), PyDateTimeAPI->DeltaType)
#define PyDelta_CheckExact(op) Py_IS_TYPE((PyObject *)(op), PyDateTimeAPI->DeltaType)

#define PyTZInfo_Check(op) _PyPon_DateTime_IsInstance((PyObject *)(op), PyDateTimeAPI->TZInfoType)
#define PyTZInfo_CheckExact(op) Py_IS_TYPE((PyObject *)(op), PyDateTimeAPI->TZInfoType)

#define PyDate_FromDate(year, month, day) \
    PyDateTimeAPI->Date_FromDate((year), (month), (day), PyDateTimeAPI->DateType)

#define PyDateTime_FromDateAndTime(year, month, day, hour, min, sec, usec) \
    PyDateTimeAPI->DateTime_FromDateAndTime((year), (month), (day), (hour), \
        (min), (sec), (usec), Py_None, PyDateTimeAPI->DateTimeType)

#define PyDateTime_FromDateAndTimeAndFold(year, month, day, hour, min, sec, usec, fold) \
    PyDateTimeAPI->DateTime_FromDateAndTimeAndFold((year), (month), (day), (hour), \
        (min), (sec), (usec), Py_None, (fold), PyDateTimeAPI->DateTimeType)

#define PyTime_FromTime(hour, minute, second, usecond) \
    PyDateTimeAPI->Time_FromTime((hour), (minute), (second), (usecond), \
        Py_None, PyDateTimeAPI->TimeType)

#define PyTime_FromTimeAndFold(hour, minute, second, usecond, fold) \
    PyDateTimeAPI->Time_FromTimeAndFold((hour), (minute), (second), (usecond), \
        Py_None, (fold), PyDateTimeAPI->TimeType)

#define PyDelta_FromDSU(days, seconds, useconds) \
    PyDateTimeAPI->Delta_FromDelta((days), (seconds), (useconds), 1, \
        PyDateTimeAPI->DeltaType)

#define PyTimeZone_FromOffset(offset) \
    PyDateTimeAPI->TimeZone_FromTimeZone((offset), NULL)

#define PyTimeZone_FromOffsetAndName(offset, name) \
    PyDateTimeAPI->TimeZone_FromTimeZone((offset), (name))

#define PyDateTime_FromTimestamp(args) \
    PyDateTimeAPI->DateTime_FromTimestamp((PyObject *)PyDateTimeAPI->DateTimeType, (args), NULL)

#define PyDate_FromTimestamp(args) \
    PyDateTimeAPI->Date_FromTimestamp((PyObject *)PyDateTimeAPI->DateType, (args))

#define PyDateTime_GET_YEAR(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "year")
#define PyDateTime_GET_MONTH(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "month")
#define PyDateTime_GET_DAY(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "day")

#define PyDateTime_DATE_GET_HOUR(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "hour")
#define PyDateTime_DATE_GET_MINUTE(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "minute")
#define PyDateTime_DATE_GET_SECOND(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "second")
#define PyDateTime_DATE_GET_MICROSECOND(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "microsecond")
#define PyDateTime_DATE_GET_FOLD(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "fold")

#define PyDateTime_TIME_GET_HOUR(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "hour")
#define PyDateTime_TIME_GET_MINUTE(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "minute")
#define PyDateTime_TIME_GET_SECOND(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "second")
#define PyDateTime_TIME_GET_MICROSECOND(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "microsecond")
#define PyDateTime_TIME_GET_FOLD(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "fold")

#define PyDateTime_DELTA_GET_DAYS(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "days")
#define PyDateTime_DELTA_GET_SECONDS(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "seconds")
#define PyDateTime_DELTA_GET_MICROSECONDS(o) _PyPon_DateTime_GetAttrInt((PyObject *)(o), "microseconds")

#endif /* !defined(_PY_DATETIME_IMPL) */

#ifdef __cplusplus
}
#endif

#endif /* PON_DATETIME_H */
