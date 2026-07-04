"""Pure-Python multibyte codec bases for Pon's native iconv-backed codecs."""

class MultibyteIncrementalEncoder:
    def __init__(self, errors="strict"):
        self.errors = errors

    def encode(self, input, final=False):
        return self.codec.encode(input, self.errors)[0]

    def reset(self):
        pass

    def getstate(self):
        return 0

    def setstate(self, state):
        if state != 0:
            raise ValueError("illegal state argument")

class MultibyteIncrementalDecoder:
    def __init__(self, errors="strict"):
        self.errors = errors
        self.buffer = b""

    def decode(self, input, final=False):
        data = self.buffer + bytes(input)
        try:
            output, consumed = self.codec.decode(data, self.errors)
        except UnicodeDecodeError:
            if final:
                self.buffer = b""
                raise
            # Keep one trailing byte for stateful and double-byte encodings when
            # a strict decode reports an incomplete final sequence.  Full error
            # recovery belongs in the native codec engine; this mirrors the
            # incremental contract without fabricating replacement text.
            if data:
                output, consumed = self.codec.decode(data[:-1], self.errors)
                self.buffer = data[-1:]
                return output
            self.buffer = data
            return ""
        self.buffer = data[consumed:]
        return output

    def reset(self):
        self.buffer = b""

    def getstate(self):
        return (self.buffer, 0)

    def setstate(self, state):
        buffer, flag = state
        if flag != 0:
            raise ValueError("illegal state argument")
        self.buffer = bytes(buffer)

class MultibyteStreamReader:
    def decode(self, input, errors="strict"):
        return self.codec.decode(input, errors)

class MultibyteStreamWriter:
    def encode(self, input, errors="strict"):
        return self.codec.encode(input, errors)
