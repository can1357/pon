import unittest
import test.test_augassign as m
loader = unittest.TestLoader()
suite = loader.loadTestsFromModule(m)
print("collected", suite.countTestCases())
import test.test_int_literal as m2
print("collected", loader.loadTestsFromModule(m2).countTestCases())
