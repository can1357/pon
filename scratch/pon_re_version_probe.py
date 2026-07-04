import re
pat = r"""
    v?+
    (?a:(?:(?P<epoch>[0-9]+)!)?+(?P<release>[0-9]+(?:\\.[0-9]+)*+)(?P<pre>[._-]?+(?P<pre_l>alpha|a|beta|b|preview|pre|c|rc)[._-]?+(?P<pre_n>[0-9]+)?)?+(?P<post>(?:-(?P<post_n1>[0-9]+))|(?:(?:[._-]?(?P<post_l>post|rev|r)[._-]?(?P<post_n2>[0-9]+)?)))?+(?P<dev>[._-]?+(?P<dev_l>dev)[._-]?+(?P<dev_n>[0-9]+)?)?+)(?a:\\+(?P<local>[a-z0-9]+(?:[._-][a-z0-9]+)*))?+
"""
rx = re.compile(r"\s*" + pat + r"\s*", re.VERBOSE | re.IGNORECASE)
print(bool(rx.fullmatch("0.dev0")))
