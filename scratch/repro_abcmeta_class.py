from abc import ABCMeta, abstractmethod
class H(metaclass=ABCMeta):
    @abstractmethod
    def __hash__(self):
        return 0
print(H.__abstractmethods__)
