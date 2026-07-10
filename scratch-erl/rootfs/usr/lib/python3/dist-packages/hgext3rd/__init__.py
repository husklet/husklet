# name space package to host third party extensions
import pkgutil

__path__ = pkgutil.extend_path(__path__, __name__)
