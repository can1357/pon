import enum

import http.client
from enum import StrEnum
import email.utils
import re

class Token(StrEnum):
    OK = 'ok'

print('StrEnum', Token.OK, Token.OK.value)
print('RegexFlag', re.RegexFlag.IGNORECASE)
print('global_enum', callable(enum.global_enum), enum.global_enum.__name__)
