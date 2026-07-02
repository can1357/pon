# unittest's loader + TestCase execution engine + TestResult bookkeeping,
# printed deterministically to stdout.  TextTestRunner is deliberately absent:
# its summary goes to stderr with a wall-clock timing line ("Ran 2 tests in
# 0.001s"), which can never be differential-stable, and the recorded failure
# entries carry traceback text whose frame formatting is interpreter-specific
# — so this module asserts the structured surface only (names, counts, ids).

import unittest


class MyTests(unittest.TestCase):
    def test_pass(self):
        self.assertEqual(1 + 1, 2)

    def test_fail(self):
        self.assertEqual(1, 2)

    def test_error(self):
        raise ValueError("deliberate")


loader = unittest.TestLoader()
print("names", loader.getTestCaseNames(MyTests))

suite = loader.loadTestsFromTestCase(MyTests)
print("count", suite.countTestCases())

result = unittest.TestResult()
suite.run(result)
print("run", result.testsRun, "failures", len(result.failures), "errors", len(result.errors))
print("successful", result.wasSuccessful())
for test, _tb_text in result.failures:
    print("failure id", test.id())
for test, _tb_text in result.errors:
    print("error id", test.id())
