# perf.py - performance test routines
'''helper extension to measure performance

Configurations
==============

``perf``
--------

``all-timing``
    When set, additional statistics will be reported for each benchmark: best,
    worst, median average. If not set only the best timing is reported
    (default: off).

``presleep``
  number of second to wait before any group of runs (default: 1)

``pre-run``
  number of run to perform before starting measurement.

``profile-benchmark``
  Enable profiling for the benchmarked section.
  (The first iteration is benchmarked)

``run-limits``
  Control the number of runs each benchmark will perform. The option value
  should be a list of `<time>-<numberofrun>` pairs. After each run the
  conditions are considered in order with the following logic:

      If benchmark has been running for <time> seconds, and we have performed
      <numberofrun> iterations, stop the benchmark,

  The default value is: `3.0-100, 10.0-3`

``stub``
    When set, benchmarks will only be run once, useful for testing
    (default: off)
'''

# "historical portability" policy of perf.py:
#
# We have to do:
# - make perf.py "loadable" with as wide Mercurial version as possible
#   This doesn't mean that perf commands work correctly with that Mercurial.
#   BTW, perf.py itself has been available since 1.1 (or eb240755386d).
# - make historical perf command work correctly with as wide Mercurial
#   version as possible
#
# We have to do, if possible with reasonable cost:
# - make recent perf command for historical feature work correctly
#   with early Mercurial
#
# We don't have to do:
# - make perf command for recent feature work correctly with early
#   Mercurial

import contextlib
import functools
import gc
import os
import random
import shutil
import struct
import sys
import tempfile
import threading
import time

import mercurial.revlog
from mercurial import (
    changegroup,
    cmdutil,
    commands,
    copies,
    error,
    extensions,
    hg,
    mdiff,
    merge,
    util,
)

# for "historical portability":
# try to import modules separately (in dict order), and ignore
# failure, because these aren't available with early Mercurial
try:
    from mercurial import branchmap  # since 2.5 (or bcee63733aad)
except ImportError:
    pass
try:
    from mercurial import obsolete  # since 2.3 (or ad0d6c2b3279)
except ImportError:
    pass
try:
    from mercurial import registrar  # since 3.7 (or 37d50250b696)

    dir(registrar)  # forcibly load it
except ImportError:
    registrar = None
try:
    from mercurial import repoview  # since 2.5 (or 3a6ddacb7198)
except ImportError:
    pass
try:
    from mercurial.utils import repoviewutil  # since 5.0
except ImportError:
    repoviewutil = None
try:
    from mercurial import scmutil  # since 1.9 (or 8b252e826c68)
except ImportError:
    pass
try:
    from mercurial import setdiscovery  # since 1.9 (or cb98fed52495)
except ImportError:
    pass

try:
    from mercurial import profiling
except ImportError:
    profiling = None

try:
    from mercurial.revlogutils import constants as revlog_constants

    perf_rl_kind = (revlog_constants.KIND_OTHER, b'created-by-perf')

    def revlog(opener, *args, **kwargs):
        return mercurial.revlog.revlog(opener, perf_rl_kind, *args, **kwargs)


except (ImportError, AttributeError):
    perf_rl_kind = None

    def revlog(opener, *args, **kwargs):
        return mercurial.revlog.revlog(opener, *args, **kwargs)


def identity(a):
    return a


try:
    from mercurial import pycompat

    getargspec = pycompat.getargspec  # added to module after 4.5
    _byteskwargs = pycompat.byteskwargs  # since 4.1 (or fbc3f73dc802)
    _sysstr = pycompat.sysstr  # since 4.0 (or 2219f4f82ede)
    _bytestr = pycompat.bytestr  # since 4.2 (or b70407bd84d5)
    _xrange = pycompat.xrange  # since 4.8 (or 7eba8f83129b)
    fsencode = pycompat.fsencode  # since 3.9 (or f4a5e0e86a7e)
    if pycompat.ispy3:
        _maxint = sys.maxsize  # per py3 docs for replacing maxint
    else:
        _maxint = sys.maxint
except (NameError, ImportError, AttributeError):
    import inspect

    getargspec = inspect.getargspec
    _byteskwargs = identity
    _bytestr = str
    fsencode = identity  # no py3 support
    _maxint = sys.maxint  # no py3 support
    _sysstr = lambda x: x  # no py3 support
    _xrange = xrange

try:
    # 4.7+
    queue = pycompat.queue.Queue
except (NameError, AttributeError, ImportError):
    # <4.7.
    try:
        queue = pycompat.queue
    except (NameError, AttributeError, ImportError):
        import Queue as queue

try:
    from mercurial import logcmdutil

    makelogtemplater = logcmdutil.maketemplater
except (AttributeError, ImportError):
    try:
        makelogtemplater = cmdutil.makelogtemplater
    except (AttributeError, ImportError):
        makelogtemplater = None

# for "historical portability":
# define util.safehasattr forcibly, because util.safehasattr has been
# available since 1.9.3 (or 94b200a11cf7)
_undefined = object()


def safehasattr(thing, attr):
    return getattr(thing, _sysstr(attr), _undefined) is not _undefined


setattr(util, 'safehasattr', safehasattr)

# for "historical portability":
# define util.timer forcibly, because util.timer has been available
# since ae5d60bb70c9
if safehasattr(time, 'perf_counter'):
    util.timer = time.perf_counter
elif os.name == b'nt':
    util.timer = time.clock
else:
    util.timer = time.time

# for "historical portability":
# use locally defined empty option list, if formatteropts isn't
# available, because commands.formatteropts has been available since
# 3.2 (or 7a7eed5176a4), even though formatting itself has been
# available since 2.2 (or ae5f92e154d3)
formatteropts = getattr(
    cmdutil, "formatteropts", getattr(commands, "formatteropts", [])
)

# for "historical portability":
# use locally defined option list, if debugrevlogopts isn't available,
# because commands.debugrevlogopts has been available since 3.7 (or
# 5606f7d0d063), even though cmdutil.openrevlog() has been available
# since 1.9 (or a79fea6b3e77).
revlogopts = getattr(
    cmdutil,
    "debugrevlogopts",
    getattr(
        commands,
        "debugrevlogopts",
        [
            (b'c', b'changelog', False, b'open changelog'),
            (b'm', b'manifest', False, b'open manifest'),
            (b'', b'dir', False, b'open directory manifest'),
        ],
    ),
)

cmdtable = {}

# for "historical portability":
# define parsealiases locally, because cmdutil.parsealiases has been
# available since 1.5 (or 6252852b4332)
def parsealiases(cmd):
    return cmd.split(b"|")


if safehasattr(registrar, 'command'):
    command = registrar.command(cmdtable)
elif safehasattr(cmdutil, 'command'):
    command = cmdutil.command(cmdtable)
    if 'norepo' not in getargspec(command).args:
        # for "historical portability":
        # wrap original cmdutil.command, because "norepo" option has
        # been available since 3.1 (or 75a96326cecb)
        _command = command

        def command(name, options=(), synopsis=None, norepo=False):
            if norepo:
                commands.norepo += b' %s' % b' '.join(parsealiases(name))
            return _command(name, list(options), synopsis)


else:
    # for "historical portability":
    # define "@command" annotation locally, because cmdutil.command
    # has been available since 1.9 (or 2daa5179e73f)
    def command(name, options=(), synopsis=None, norepo=False):
        def decorator(func):
            if synopsis:
                cmdtable[name] = func, list(options), synopsis
            else:
                cmdtable[name] = func, list(options)
            if norepo:
                commands.norepo += b' %s' % b' '.join(parsealiases(name))
            return func

        return decorator


try:
    import mercurial.registrar
    import mercurial.configitems

    configtable = {}
    configitem = mercurial.registrar.configitem(configtable)
    configitem(
        b'perf',
        b'presleep',
        default=mercurial.configitems.dynamicdefault,
        experimental=True,
    )
    configitem(
        b'perf',
        b'stub',
        default=mercurial.configitems.dynamicdefault,
        experimental=True,
    )
    configitem(
        b'perf',
        b'parentscount',
        default=mercurial.configitems.dynamicdefault,
        experimental=True,
    )
    configitem(
        b'perf',
        b'all-timing',
        default=mercurial.configitems.dynamicdefault,
        experimental=True,
    )
    configitem(
        b'perf',
        b'pre-run',
        default=mercurial.configitems.dynamicdefault,
    )
    configitem(
        b'perf',
        b'profile-benchmark',
        default=mercurial.configitems.dynamicdefault,
    )
    configitem(
        b'perf',
        b'run-limits',
        default=mercurial.configitems.dynamicdefault,
        experimental=True,
    )
except (ImportError, AttributeError):
    pass
except TypeError:
    # compatibility fix for a11fd395e83f
    # hg version: 5.2
    configitem(
        b'perf',
        b'presleep',
        default=mercurial.configitems.dynamicdefault,
    )
    configitem(
        b'perf',
        b'stub',
        default=mercurial.configitems.dynamicdefault,
    )
    configitem(
        b'perf',
        b'parentscount',
        default=mercurial.configitems.dynamicdefault,
    )
    configitem(
        b'perf',
        b'all-timing',
        default=mercurial.configitems.dynamicdefault,
    )
    configitem(
        b'perf',
        b'pre-run',
        default=mercurial.configitems.dynamicdefault,
    )
    configitem(
        b'perf',
        b'profile-benchmark',
        default=mercurial.configitems.dynamicdefault,
    )
    configitem(
        b'perf',
        b'run-limits',
        default=mercurial.configitems.dynamicdefault,
    )


def getlen(ui):
    if ui.configbool(b"perf", b"stub", False):
        return lambda x: 1
    return len


class noop:
    """dummy context manager"""

    def __enter__(self):
        pass

    def __exit__(self, *args):
        pass


NOOPCTX = noop()


def gettimer(ui, opts=None):
    """return a timer function and formatter: (timer, formatter)

    This function exists to gather the creation of formatter in a single
    place instead of duplicating it in all performance commands."""

    # enforce an idle period before execution to counteract power management
    # experimental config: perf.presleep
    time.sleep(getint(ui, b"perf", b"presleep", 1))

    if opts is None:
        opts = {}
    # redirect all to stderr unless buffer api is in use
    if not ui._buffers:
        ui = ui.copy()
        uifout = safeattrsetter(ui, b'fout', ignoremissing=True)
        if uifout:
            # for "historical portability":
            # ui.fout/ferr have been available since 1.9 (or 4e1ccd4c2b6d)
            uifout.set(ui.ferr)

    # get a formatter
    uiformatter = getattr(ui, 'formatter', None)
    if uiformatter:
        fm = uiformatter(b'perf', opts)
    else:
        # for "historical portability":
        # define formatter locally, because ui.formatter has been
        # available since 2.2 (or ae5f92e154d3)
        from mercurial import node

        class defaultformatter:
            """Minimized composition of baseformatter and plainformatter"""

            def __init__(self, ui, topic, opts):
                self._ui = ui
                if ui.debugflag:
                    self.hexfunc = node.hex
                else:
                    self.hexfunc = node.short

            def __nonzero__(self):
                return False

            __bool__ = __nonzero__

            def startitem(self):
                pass

            def data(self, **data):
                pass

            def write(self, fields, deftext, *fielddata, **opts):
                self._ui.write(deftext % fielddata, **opts)

            def condwrite(self, cond, fields, deftext, *fielddata, **opts):
                if cond:
                    self._ui.write(deftext % fielddata, **opts)

            def plain(self, text, **opts):
                self._ui.write(text, **opts)

            def end(self):
                pass

        fm = defaultformatter(ui, b'perf', opts)

    # stub function, runs code only once instead of in a loop
    # experimental config: perf.stub
    if ui.configbool(b"perf", b"stub", False):
        return functools.partial(stub_timer, fm), fm

    # experimental config: perf.all-timing
    displayall = ui.configbool(b"perf", b"all-timing", False)

    # experimental config: perf.run-limits
    limitspec = ui.configlist(b"perf", b"run-limits", [])
    limits = []
    for item in limitspec:
        parts = item.split(b'-', 1)
        if len(parts) < 2:
            ui.warn((b'malformatted run limit entry, missing "-": %s\n' % item))
            continue
        try:
            time_limit = float(_sysstr(parts[0]))
        except ValueError as e:
            ui.warn(
                (
                    b'malformatted run limit entry, %s: %s\n'
                    % (_bytestr(e), item)
                )
            )
            continue
        try:
            run_limit = int(_sysstr(parts[1]))
        except ValueError as e:
            ui.warn(
                (
                    b'malformatted run limit entry, %s: %s\n'
                    % (_bytestr(e), item)
                )
            )
            continue
        limits.append((time_limit, run_limit))
    if not limits:
        limits = DEFAULTLIMITS

    profiler = None
    if profiling is not None:
        if ui.configbool(b"perf", b"profile-benchmark", False):
            profiler = profiling.profile(ui)

    prerun = getint(ui, b"perf", b"pre-run", 0)
    t = functools.partial(
        _timer,
        fm,
        displayall=displayall,
        limits=limits,
        prerun=prerun,
        profiler=profiler,
    )
    return t, fm


def stub_timer(fm, func, setup=None, title=None):
    if setup is not None:
        setup()
    func()


@contextlib.contextmanager
def timeone():
    r = []
    ostart = os.times()
    cstart = util.timer()
    yield r
    cstop = util.timer()
    ostop = os.times()
    a, b = ostart, ostop
    r.append((cstop - cstart, b[0] - a[0], b[1] - a[1]))


# list of stop condition (elapsed time, minimal run count)
DEFAULTLIMITS = (
    (3.0, 100),
    (10.0, 3),
)


def _timer(
    fm,
    func,
    setup=None,
    title=None,
    displayall=False,
    limits=DEFAULTLIMITS,
    prerun=0,
    profiler=None,
):
    gc.collect()
    results = []
    begin = util.timer()
    count = 0
    if profiler is None:
        profiler = NOOPCTX
    for i in range(prerun):
        if setup is not None:
            setup()
        func()
    keepgoing = True
    while keepgoing:
        if setup is not None:
            setup()
        with profiler:
            with timeone() as item:
                r = func()
        profiler = NOOPCTX
        count += 1
        results.append(item[0])
        cstop = util.timer()
        # Look for a stop condition.
        elapsed = cstop - begin
        for t, mincount in limits:
            if elapsed >= t and count >= mincount:
                keepgoing = False
                break

    formatone(fm, results, title=title, result=r, displayall=displayall)


def formatone(fm, timings, title=None, result=None, displayall=False):

    count = len(timings)

    fm.startitem()

    if title:
        fm.write(b'title', b'! %s\n', title)
    if result:
        fm.write(b'result', b'! result: %s\n', result)

    def display(role, entry):
        prefix = b''
        if role != b'best':
            prefix = b'%s.' % role
        fm.plain(b'!')
        fm.write(prefix + b'wall', b' wall %f', entry[0])
        fm.write(prefix + b'comb', b' comb %f', entry[1] + entry[2])
        fm.write(prefix + b'user', b' user %f', entry[1])
        fm.write(prefix + b'sys', b' sys %f', entry[2])
        fm.write(prefix + b'count', b' (%s of %%d)' % role, count)
        fm.plain(b'\n')

    timings.sort()
    min_val = timings[0]
    display(b'best', min_val)
    if displayall:
        max_val = timings[-1]
        display(b'max', max_val)
        avg = tuple([sum(x) / count for x in zip(*timings)])
        display(b'avg', avg)
        median = timings[len(timings) // 2]
        display(b'median', median)


# utilities for historical portability


def getint(ui, section, name, default):
    # for "historical portability":
    # ui.configint has been available since 1.9 (or fa2b596db182)
    v = ui.config(section, name, None)
    if v is None:
        return default
    try:
        return int(v)
    except ValueError:
        raise error.ConfigError(
            b"%s.%s is not an integer ('%s')" % (section, name, v)
        )


def safeattrsetter(obj, name, ignoremissing=False):
    """Ensure that 'obj' has 'name' attribute before subsequent setattr

    This function is aborted, if 'obj' doesn't have 'name' attribute
    at runtime. This avoids overlooking removal of an attribute, which
    breaks assumption of performance measurement, in the future.

    This function returns the object to (1) assign a new value, and
    (2) restore an original value to the attribute.

    If 'ignoremissing' is true, missing 'name' attribute doesn't cause
    abortion, and this function returns None. This is useful to
    examine an attribute, which isn't ensured in all Mercurial
    versions.
    """
    if not util.safehasattr(obj, name):
        if ignoremissing:
            return None
        raise error.Abort(
            (
                b"missing attribute %s of %s might break assumption"
                b" of performance measurement"
            )
            % (name, obj)
        )

    origvalue = getattr(obj, _sysstr(name))

    class attrutil:
        def set(self, newvalue):
            setattr(obj, _sysstr(name), newvalue)

        def restore(self):
            setattr(obj, _sysstr(name), origvalue)

    return attrutil()


# utilities to examine each internal API changes


def getbranchmapsubsettable():
    # for "historical portability":
    # subsettable is defined in:
    # - branchmap since 2.9 (or 175c6fd8cacc)
    # - repoview since 2.5 (or 59a9f18d4587)
    # - repoviewutil since 5.0
    for mod in (branchmap, repoview, repoviewutil):
        subsettable = getattr(mod, 'subsettable', None)
        if subsettable:
            return subsettable

    # bisecting in bcee63733aad::59a9f18d4587 can reach here (both
    # branchmap and repoview modules exist, but subsettable attribute
    # doesn't)
    raise error.Abort(
        b"perfbranchmap not available with this Mercurial",
        hint=b"use 2.5 or later",
    )


def getsvfs(repo):
    """Return appropriate object to access files under .hg/store"""
    # for "historical portability":
    # repo.svfs has been available since 2.3 (or 7034365089bf)
    svfs = getattr(repo, 'svfs', None)
    if svfs:
        return svfs
    else:
        return getattr(repo, 'sopener')


def getvfs(repo):
    """Return appropriate object to access files under .hg"""
    # for "historical portability":
    # repo.vfs has been available since 2.3 (or 7034365089bf)
    vfs = getattr(repo, 'vfs', None)
    if vfs:
        return vfs
    else:
        return getattr(repo, 'opener')


def repocleartagscachefunc(repo):
    """Return the function to clear tags cache according to repo internal API"""
    if util.safehasattr(repo, b'_tagscache'):  # since 2.0 (or 9dca7653b525)
        # in this case, setattr(repo, '_tagscache', None) or so isn't
        # correct way to clear tags cache, because existing code paths
        # expect _tagscache to be a structured object.
        def clearcache():
            # _tagscache has been filteredpropertycache since 2.5 (or
            # 98c867ac1330), and delattr() can't work in such case
            if '_tagscache' in vars(repo):
                del repo.__dict__['_tagscache']

        return clearcache

    repotags = safeattrsetter(repo, b'_tags', ignoremissing=True)
    if repotags:  # since 1.4 (or 5614a628d173)
        return lambda: repotags.set(None)

    repotagscache = safeattrsetter(repo, b'tagscache', ignoremissing=True)
    if repotagscache:  # since 0.6 (or d7df759d0e97)
        return lambda: repotagscache.set(None)

    # Mercurial earlier than 0.6 (or d7df759d0e97) logically reaches
    # this point, but it isn't so problematic, because:
    # - repo.tags of such Mercurial isn't "callable", and repo.tags()
    #   in perftags() causes failure soon
    # - perf.py itself has been available since 1.1 (or eb240755386d)
    raise error.Abort(b"tags API of this hg command is unknown")


# utilities to clear cache


def clearfilecache(obj, attrname):
    unfiltered = getattr(obj, 'unfiltered', None)
    if unfiltered is not None:
        obj = obj.unfiltered()
    if attrname in vars(obj):
        delattr(obj, attrname)
    obj._filecache.pop(attrname, None)


def clearchangelog(repo):
    if repo is not repo.unfiltered():
        object.__setattr__(repo, '_clcachekey', None)
        object.__setattr__(repo, '_clcache', None)
    clearfilecache(repo.unfiltered(), 'changelog')


# perf commands


@command(b'perf::walk|perfwalk', formatteropts)
def perfwalk(ui, repo, *pats, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    m = scmutil.match(repo[None], pats, {})
    timer(
        lambda: len(
            list(
                repo.dirstate.walk(m, subrepos=[], unknown=True, ignored=False)
            )
        )
    )
    fm.end()


@command(b'perf::annotate|perfannotate', formatteropts)
def perfannotate(ui, repo, f, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    fc = repo[b'.'][f]
    timer(lambda: len(fc.annotate(True)))
    fm.end()


@command(
    b'perf::status|perfstatus',
    [
        (b'u', b'unknown', False, b'ask status to look for unknown files'),
        (b'', b'dirstate', False, b'benchmark the internal dirstate call'),
    ]
    + formatteropts,
)
def perfstatus(ui, repo, **opts):
    """benchmark the performance of a single status call

    The repository data are preserved between each call.

    By default, only the status of the tracked file are requested. If
    `--unknown` is passed, the "unknown" files are also tracked.
    """
    opts = _byteskwargs(opts)
    # m = match.always(repo.root, repo.getcwd())
    # timer(lambda: sum(map(len, repo.dirstate.status(m, [], False, False,
    #                                                False))))
    timer, fm = gettimer(ui, opts)
    if opts[b'dirstate']:
        dirstate = repo.dirstate
        m = scmutil.matchall(repo)
        unknown = opts[b'unknown']

        def status_dirstate():
            s = dirstate.status(
                m, subrepos=[], ignored=False, clean=False, unknown=unknown
            )
            sum(map(bool, s))

        timer(status_dirstate)
    else:
        timer(lambda: sum(map(len, repo.status(unknown=opts[b'unknown']))))
    fm.end()


@command(b'perf::addremove|perfaddremove', formatteropts)
def perfaddremove(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    try:
        oldquiet = repo.ui.quiet
        repo.ui.quiet = True
        matcher = scmutil.match(repo[None])
        opts[b'dry_run'] = True
        if 'uipathfn' in getargspec(scmutil.addremove).args:
            uipathfn = scmutil.getuipathfn(repo)
            timer(lambda: scmutil.addremove(repo, matcher, b"", uipathfn, opts))
        else:
            timer(lambda: scmutil.addremove(repo, matcher, b"", opts))
    finally:
        repo.ui.quiet = oldquiet
        fm.end()


def clearcaches(cl):
    # behave somewhat consistently across internal API changes
    if util.safehasattr(cl, b'clearcaches'):
        cl.clearcaches()
    elif util.safehasattr(cl, b'_nodecache'):
        # <= hg-5.2
        from mercurial.node import nullid, nullrev

        cl._nodecache = {nullid: nullrev}
        cl._nodepos = None


@command(b'perf::heads|perfheads', formatteropts)
def perfheads(ui, repo, **opts):
    """benchmark the computation of a changelog heads"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    cl = repo.changelog

    def s():
        clearcaches(cl)

    def d():
        len(cl.headrevs())

    timer(d, setup=s)
    fm.end()


@command(
    b'perf::tags|perftags',
    formatteropts
    + [
        (b'', b'clear-revlogs', False, b'refresh changelog and manifest'),
    ],
)
def perftags(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    repocleartagscache = repocleartagscachefunc(repo)
    clearrevlogs = opts[b'clear_revlogs']

    def s():
        if clearrevlogs:
            clearchangelog(repo)
            clearfilecache(repo.unfiltered(), 'manifest')
        repocleartagscache()

    def t():
        return len(repo.tags())

    timer(t, setup=s)
    fm.end()


@command(b'perf::ancestors|perfancestors', formatteropts)
def perfancestors(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    heads = repo.changelog.headrevs()

    def d():
        for a in repo.changelog.ancestors(heads):
            pass

    timer(d)
    fm.end()


@command(b'perf::ancestorset|perfancestorset', formatteropts)
def perfancestorset(ui, repo, revset, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    revs = repo.revs(revset)
    heads = repo.changelog.headrevs()

    def d():
        s = repo.changelog.ancestors(heads)
        for rev in revs:
            rev in s

    timer(d)
    fm.end()


@command(
    b'perf::delta-find',
    revlogopts + formatteropts,
    b'-c|-m|FILE REV',
)
def perf_delta_find(ui, repo, arg_1, arg_2=None, **opts):
    """benchmark the process of finding a valid delta for a revlog revision

    When a revlog receives a new revision (e.g. from a commit, or from an
    incoming bundle), it searches for a suitable delta-base to produce a delta.
    This perf command measures how much time we spend in this process. It
    operates on an already stored revision.

    See `hg help debug-delta-find` for another related command.
    """
    from mercurial import revlogutils
    import mercurial.revlogutils.deltas as deltautil

    opts = _byteskwargs(opts)
    if arg_2 is None:
        file_ = None
        rev = arg_1
    else:
        file_ = arg_1
        rev = arg_2

    repo = repo.unfiltered()

    timer, fm = gettimer(ui, opts)

    rev = int(rev)

    revlog = cmdutil.openrevlog(repo, b'perf::delta-find', file_, opts)

    deltacomputer = deltautil.deltacomputer(revlog)

    node = revlog.node(rev)
    p1r, p2r = revlog.parentrevs(rev)
    p1 = revlog.node(p1r)
    p2 = revlog.node(p2r)
    full_text = revlog.revision(rev)
    textlen = len(full_text)
    cachedelta = None
    flags = revlog.flags(rev)

    revinfo = revlogutils.revisioninfo(
        node,
        p1,
        p2,
        [full_text],  # btext
        textlen,
        cachedelta,
        flags,
    )

    # Note: we should probably purge the potential caches (like the full
    # manifest cache) between runs.
    def find_one():
        with revlog._datafp() as fh:
            deltacomputer.finddeltainfo(revinfo, fh, target_rev=rev)

    timer(find_one)
    fm.end()


@command(b'perf::discovery|perfdiscovery', formatteropts, b'PATH')
def perfdiscovery(ui, repo, path, **opts):
    """benchmark discovery between local repo and the peer at given path"""
    repos = [repo, None]
    timer, fm = gettimer(ui, opts)

    try:
        from mercurial.utils.urlutil import get_unique_pull_path

        path = get_unique_pull_path(b'perfdiscovery', repo, ui, path)[0]
    except ImportError:
        path = ui.expandpath(path)

    def s():
        repos[1] = hg.peer(ui, opts, path)

    def d():
        setdiscovery.findcommonheads(ui, *repos)

    timer(d, setup=s)
    fm.end()


@command(
    b'perf::bookmarks|perfbookmarks',
    formatteropts
    + [
        (b'', b'clear-revlogs', False, b'refresh changelog and manifest'),
    ],
)
def perfbookmarks(ui, repo, **opts):
    """benchmark parsing bookmarks from disk to memory"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)

    clearrevlogs = opts[b'clear_revlogs']

    def s():
        if clearrevlogs:
            clearchangelog(repo)
        clearfilecache(repo, b'_bookmarks')

    def d():
        repo._bookmarks

    timer(d, setup=s)
    fm.end()


@command(
    b'perf::bundle',
    [
        (
            b'r',
            b'rev',
            [],
            b'changesets to bundle',
            b'REV',
        ),
        (
            b't',
            b'type',
            b'none',
            b'bundlespec to use (see `hg help bundlespec`)',
            b'TYPE',
        ),
    ]
    + formatteropts,
    b'REVS',
)
def perfbundle(ui, repo, *revs, **opts):
    """benchmark the creation of a bundle from a repository

    For now, this only supports "none" compression.
    """
    try:
        from mercurial import bundlecaches

        parsebundlespec = bundlecaches.parsebundlespec
    except ImportError:
        from mercurial import exchange

        parsebundlespec = exchange.parsebundlespec

    from mercurial import discovery
    from mercurial import bundle2

    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)

    cl = repo.changelog
    revs = list(revs)
    revs.extend(opts.get(b'rev', ()))
    revs = scmutil.revrange(repo, revs)
    if not revs:
        raise error.Abort(b"not revision specified")
    # make it a consistent set (ie: without topological gaps)
    old_len = len(revs)
    revs = list(repo.revs(b"%ld::%ld", revs, revs))
    if old_len != len(revs):
        new_count = len(revs) - old_len
        msg = b"add %d new revisions to make it a consistent set\n"
        ui.write_err(msg % new_count)

    targets = [cl.node(r) for r in repo.revs(b"heads(::%ld)", revs)]
    bases = [cl.node(r) for r in repo.revs(b"heads(::%ld - %ld)", revs, revs)]
    outgoing = discovery.outgoing(repo, bases, targets)

    bundle_spec = opts.get(b'type')

    bundle_spec = parsebundlespec(repo, bundle_spec, strict=False)

    cgversion = bundle_spec.params.get(b"cg.version")
    if cgversion is None:
        if bundle_spec.version == b'v1':
            cgversion = b'01'
        if bundle_spec.version == b'v2':
            cgversion = b'02'
    if cgversion not in changegroup.supportedoutgoingversions(repo):
        err = b"repository does not support bundle version %s"
        raise error.Abort(err % cgversion)

    if cgversion == b'01':  # bundle1
        bversion = b'HG10' + bundle_spec.wirecompression
        bcompression = None
    elif cgversion in (b'02', b'03'):
        bversion = b'HG20'
        bcompression = bundle_spec.wirecompression
    else:
        err = b'perf::bundle: unexpected changegroup version %s'
        raise error.ProgrammingError(err % cgversion)

    if bcompression is None:
        bcompression = b'UN'

    if bcompression != b'UN':
        err = b'perf::bundle: compression currently unsupported: %s'
        raise error.ProgrammingError(err % bcompression)

    def do_bundle():
        bundle2.writenewbundle(
            ui,
            repo,
            b'perf::bundle',
            os.devnull,
            bversion,
            outgoing,
            bundle_spec.params,
        )

    timer(do_bundle)
    fm.end()


@command(b'perf::bundleread|perfbundleread', formatteropts, b'BUNDLE')
def perfbundleread(ui, repo, bundlepath, **opts):
    """Benchmark reading of bundle files.

    This command is meant to isolate the I/O part of bundle reading as
    much as possible.
    """
    from mercurial import (
        bundle2,
        exchange,
        streamclone,
    )

    opts = _byteskwargs(opts)

    def makebench(fn):
        def run():
            with open(bundlepath, b'rb') as fh:
                bundle = exchange.readbundle(ui, fh, bundlepath)
                fn(bundle)

        return run

    def makereadnbytes(size):
        def run():
            with open(bundlepath, b'rb') as fh:
                bundle = exchange.readbundle(ui, fh, bundlepath)
                while bundle.read(size):
                    pass

        return run

    def makestdioread(size):
        def run():
            with open(bundlepath, b'rb') as fh:
                while fh.read(size):
                    pass

        return run

    # bundle1

    def deltaiter(bundle):
        for delta in bundle.deltaiter():
            pass

    def iterchunks(bundle):
        for chunk in bundle.getchunks():
            pass

    # bundle2

    def forwardchunks(bundle):
        for chunk in bundle._forwardchunks():
            pass

    def iterparts(bundle):
        for part in bundle.iterparts():
            pass

    def iterpartsseekable(bundle):
        for part in bundle.iterparts(seekable=True):
            pass

    def seek(bundle):
        for part in bundle.iterparts(seekable=True):
            part.seek(0, os.SEEK_END)

    def makepartreadnbytes(size):
        def run():
            with open(bundlepath, b'rb') as fh:
                bundle = exchange.readbundle(ui, fh, bundlepath)
                for part in bundle.iterparts():
                    while part.read(size):
                        pass

        return run

    benches = [
        (makestdioread(8192), b'read(8k)'),
        (makestdioread(16384), b'read(16k)'),
        (makestdioread(32768), b'read(32k)'),
        (makestdioread(131072), b'read(128k)'),
    ]

    with open(bundlepath, b'rb') as fh:
        bundle = exchange.readbundle(ui, fh, bundlepath)

        if isinstance(bundle, changegroup.cg1unpacker):
            benches.extend(
                [
                    (makebench(deltaiter), b'cg1 deltaiter()'),
                    (makebench(iterchunks), b'cg1 getchunks()'),
                    (makereadnbytes(8192), b'cg1 read(8k)'),
                    (makereadnbytes(16384), b'cg1 read(16k)'),
                    (makereadnbytes(32768), b'cg1 read(32k)'),
                    (makereadnbytes(131072), b'cg1 read(128k)'),
                ]
            )
        elif isinstance(bundle, bundle2.unbundle20):
            benches.extend(
                [
                    (makebench(forwardchunks), b'bundle2 forwardchunks()'),
                    (makebench(iterparts), b'bundle2 iterparts()'),
                    (
                        makebench(iterpartsseekable),
                        b'bundle2 iterparts() seekable',
                    ),
                    (makebench(seek), b'bundle2 part seek()'),
                    (makepartreadnbytes(8192), b'bundle2 part read(8k)'),
                    (makepartreadnbytes(16384), b'bundle2 part read(16k)'),
                    (makepartreadnbytes(32768), b'bundle2 part read(32k)'),
                    (makepartreadnbytes(131072), b'bundle2 part read(128k)'),
                ]
            )
        elif isinstance(bundle, streamclone.streamcloneapplier):
            raise error.Abort(b'stream clone bundles not supported')
        else:
            raise error.Abort(b'unhandled bundle type: %s' % type(bundle))

    for fn, title in benches:
        timer, fm = gettimer(ui, opts)
        timer(fn, title=title)
        fm.end()


@command(
    b'perf::changegroupchangelog|perfchangegroupchangelog',
    formatteropts
    + [
        (b'', b'cgversion', b'02', b'changegroup version'),
        (b'r', b'rev', b'', b'revisions to add to changegroup'),
    ],
)
def perfchangegroupchangelog(ui, repo, cgversion=b'02', rev=None, **opts):
    """Benchmark producing a changelog group for a changegroup.

    This measures the time spent processing the changelog during a
    bundle operation. This occurs during `hg bundle` and on a server
    processing a `getbundle` wire protocol request (handles clones
    and pull requests).

    By default, all revisions are added to the changegroup.
    """
    opts = _byteskwargs(opts)
    cl = repo.changelog
    nodes = [cl.lookup(r) for r in repo.revs(rev or b'all()')]
    bundler = changegroup.getbundler(cgversion, repo)

    def d():
        state, chunks = bundler._generatechangelog(cl, nodes)
        for chunk in chunks:
            pass

    timer, fm = gettimer(ui, opts)

    # Terminal printing can interfere with timing. So disable it.
    with ui.configoverride({(b'progress', b'disable'): True}):
        timer(d)

    fm.end()


@command(b'perf::dirs|perfdirs', formatteropts)
def perfdirs(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    dirstate = repo.dirstate
    b'a' in dirstate

    def d():
        dirstate.hasdir(b'a')
        try:
            del dirstate._map._dirs
        except AttributeError:
            pass

    timer(d)
    fm.end()


@command(
    b'perf::dirstate|perfdirstate',
    [
        (
            b'',
            b'iteration',
            None,
            b'benchmark a full iteration for the dirstate',
        ),
        (
            b'',
            b'contains',
            None,
            b'benchmark a large amount of `nf in dirstate` calls',
        ),
    ]
    + formatteropts,
)
def perfdirstate(ui, repo, **opts):
    """benchmap the time of various distate operations

    By default benchmark the time necessary to load a dirstate from scratch.
    The dirstate is loaded to the point were a "contains" request can be
    answered.
    """
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    b"a" in repo.dirstate

    if opts[b'iteration'] and opts[b'contains']:
        msg = b'only specify one of --iteration or --contains'
        raise error.Abort(msg)

    if opts[b'iteration']:
        setup = None
        dirstate = repo.dirstate

        def d():
            for f in dirstate:
                pass

    elif opts[b'contains']:
        setup = None
        dirstate = repo.dirstate
        allfiles = list(dirstate)
        # also add file path that will be "missing" from the dirstate
        allfiles.extend([f[::-1] for f in allfiles])

        def d():
            for f in allfiles:
                f in dirstate

    else:

        def setup():
            repo.dirstate.invalidate()

        def d():
            b"a" in repo.dirstate

    timer(d, setup=setup)
    fm.end()


@command(b'perf::dirstatedirs|perfdirstatedirs', formatteropts)
def perfdirstatedirs(ui, repo, **opts):
    """benchmap a 'dirstate.hasdir' call from an empty `dirs` cache"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    repo.dirstate.hasdir(b"a")

    def setup():
        try:
            del repo.dirstate._map._dirs
        except AttributeError:
            pass

    def d():
        repo.dirstate.hasdir(b"a")

    timer(d, setup=setup)
    fm.end()


@command(b'perf::dirstatefoldmap|perfdirstatefoldmap', formatteropts)
def perfdirstatefoldmap(ui, repo, **opts):
    """benchmap a `dirstate._map.filefoldmap.get()` request

    The dirstate filefoldmap cache is dropped between every request.
    """
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    dirstate = repo.dirstate
    dirstate._map.filefoldmap.get(b'a')

    def setup():
        del dirstate._map.filefoldmap

    def d():
        dirstate._map.filefoldmap.get(b'a')

    timer(d, setup=setup)
    fm.end()


@command(b'perf::dirfoldmap|perfdirfoldmap', formatteropts)
def perfdirfoldmap(ui, repo, **opts):
    """benchmap a `dirstate._map.dirfoldmap.get()` request

    The dirstate dirfoldmap cache is dropped between every request.
    """
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    dirstate = repo.dirstate
    dirstate._map.dirfoldmap.get(b'a')

    def setup():
        del dirstate._map.dirfoldmap
        try:
            del dirstate._map._dirs
        except AttributeError:
            pass

    def d():
        dirstate._map.dirfoldmap.get(b'a')

    timer(d, setup=setup)
    fm.end()


@command(b'perf::dirstatewrite|perfdirstatewrite', formatteropts)
def perfdirstatewrite(ui, repo, **opts):
    """benchmap the time it take to write a dirstate on disk"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    ds = repo.dirstate
    b"a" in ds

    def setup():
        ds._dirty = True

    def d():
        ds.write(repo.currenttransaction())

    timer(d, setup=setup)
    fm.end()


def _getmergerevs(repo, opts):
    """parse command argument to return rev involved in merge

    input: options dictionnary with `rev`, `from` and `bse`
    output: (localctx, otherctx, basectx)
    """
    if opts[b'from']:
        fromrev = scmutil.revsingle(repo, opts[b'from'])
        wctx = repo[fromrev]
    else:
        wctx = repo[None]
        # we don't want working dir files to be stat'd in the benchmark, so
        # prime that cache
        wctx.dirty()
    rctx = scmutil.revsingle(repo, opts[b'rev'], opts[b'rev'])
    if opts[b'base']:
        fromrev = scmutil.revsingle(repo, opts[b'base'])
        ancestor = repo[fromrev]
    else:
        ancestor = wctx.ancestor(rctx)
    return (wctx, rctx, ancestor)


@command(
    b'perf::mergecalculate|perfmergecalculate',
    [
        (b'r', b'rev', b'.', b'rev to merge against'),
        (b'', b'from', b'', b'rev to merge from'),
        (b'', b'base', b'', b'the revision to use as base'),
    ]
    + formatteropts,
)
def perfmergecalculate(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)

    wctx, rctx, ancestor = _getmergerevs(repo, opts)

    def d():
        # acceptremote is True because we don't want prompts in the middle of
        # our benchmark
        merge.calculateupdates(
            repo,
            wctx,
            rctx,
            [ancestor],
            branchmerge=False,
            force=False,
            acceptremote=True,
            followcopies=True,
        )

    timer(d)
    fm.end()


@command(
    b'perf::mergecopies|perfmergecopies',
    [
        (b'r', b'rev', b'.', b'rev to merge against'),
        (b'', b'from', b'', b'rev to merge from'),
        (b'', b'base', b'', b'the revision to use as base'),
    ]
    + formatteropts,
)
def perfmergecopies(ui, repo, **opts):
    """measure runtime of `copies.mergecopies`"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    wctx, rctx, ancestor = _getmergerevs(repo, opts)

    def d():
        # acceptremote is True because we don't want prompts in the middle of
        # our benchmark
        copies.mergecopies(repo, wctx, rctx, ancestor)

    timer(d)
    fm.end()


@command(b'perf::pathcopies|perfpathcopies', [], b"REV REV")
def perfpathcopies(ui, repo, rev1, rev2, **opts):
    """benchmark the copy tracing logic"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    ctx1 = scmutil.revsingle(repo, rev1, rev1)
    ctx2 = scmutil.revsingle(repo, rev2, rev2)

    def d():
        copies.pathcopies(ctx1, ctx2)

    timer(d)
    fm.end()


@command(
    b'perf::phases|perfphases',
    [
        (b'', b'full', False, b'include file reading time too'),
    ],
    b"",
)
def perfphases(ui, repo, **opts):
    """benchmark phasesets computation"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    _phases = repo._phasecache
    full = opts.get(b'full')

    def d():
        phases = _phases
        if full:
            clearfilecache(repo, b'_phasecache')
            phases = repo._phasecache
        phases.invalidate()
        phases.loadphaserevs(repo)

    timer(d)
    fm.end()


@command(b'perf::phasesremote|perfphasesremote', [], b"[DEST]")
def perfphasesremote(ui, repo, dest=None, **opts):
    """benchmark time needed to analyse phases of the remote server"""
    from mercurial.node import bin
    from mercurial import (
        exchange,
        hg,
        phases,
    )

    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)

    path = ui.getpath(dest, default=(b'default-push', b'default'))
    if not path:
        raise error.Abort(
            b'default repository not configured!',
            hint=b"see 'hg help config.paths'",
        )
    dest = path.pushloc or path.loc
    ui.statusnoi18n(b'analysing phase of %s\n' % util.hidepassword(dest))
    other = hg.peer(repo, opts, dest)

    # easier to perform discovery through the operation
    op = exchange.pushoperation(repo, other)
    exchange._pushdiscoverychangeset(op)

    remotesubset = op.fallbackheads

    with other.commandexecutor() as e:
        remotephases = e.callcommand(
            b'listkeys', {b'namespace': b'phases'}
        ).result()
    del other
    publishing = remotephases.get(b'publishing', False)
    if publishing:
        ui.statusnoi18n(b'publishing: yes\n')
    else:
        ui.statusnoi18n(b'publishing: no\n')

    has_node = getattr(repo.changelog.index, 'has_node', None)
    if has_node is None:
        has_node = repo.changelog.nodemap.__contains__
    nonpublishroots = 0
    for nhex, phase in remotephases.iteritems():
        if nhex == b'publishing':  # ignore data related to publish option
            continue
        node = bin(nhex)
        if has_node(node) and int(phase):
            nonpublishroots += 1
    ui.statusnoi18n(b'number of roots: %d\n' % len(remotephases))
    ui.statusnoi18n(b'number of known non public roots: %d\n' % nonpublishroots)

    def d():
        phases.remotephasessummary(repo, remotesubset, remotephases)

    timer(d)
    fm.end()


@command(
    b'perf::manifest|perfmanifest',
    [
        (b'm', b'manifest-rev', False, b'Look up a manifest node revision'),
        (b'', b'clear-disk', False, b'clear on-disk caches too'),
    ]
    + formatteropts,
    b'REV|NODE',
)
def perfmanifest(ui, repo, rev, manifest_rev=False, clear_disk=False, **opts):
    """benchmark the time to read a manifest from disk and return a usable
    dict-like object

    Manifest caches are cleared before retrieval."""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    if not manifest_rev:
        ctx = scmutil.revsingle(repo, rev, rev)
        t = ctx.manifestnode()
    else:
        from mercurial.node import bin

        if len(rev) == 40:
            t = bin(rev)
        else:
            try:
                rev = int(rev)

                if util.safehasattr(repo.manifestlog, b'getstorage'):
                    t = repo.manifestlog.getstorage(b'').node(rev)
                else:
                    t = repo.manifestlog._revlog.lookup(rev)
            except ValueError:
                raise error.Abort(
                    b'manifest revision must be integer or full node'
                )

    def d():
        repo.manifestlog.clearcaches(clear_persisted_data=clear_disk)
        repo.manifestlog[t].read()

    timer(d)
    fm.end()


@command(b'perf::changeset|perfchangeset', formatteropts)
def perfchangeset(ui, repo, rev, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    n = scmutil.revsingle(repo, rev).node()

    def d():
        repo.changelog.read(n)
        # repo.changelog._cache = None

    timer(d)
    fm.end()


@command(b'perf::ignore|perfignore', formatteropts)
def perfignore(ui, repo, **opts):
    """benchmark operation related to computing ignore"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    dirstate = repo.dirstate

    def setupone():
        dirstate.invalidate()
        clearfilecache(dirstate, b'_ignore')

    def runone():
        dirstate._ignore

    timer(runone, setup=setupone, title=b"load")
    fm.end()


@command(
    b'perf::index|perfindex',
    [
        (b'', b'rev', [], b'revision to be looked up (default tip)'),
        (b'', b'no-lookup', None, b'do not revision lookup post creation'),
    ]
    + formatteropts,
)
def perfindex(ui, repo, **opts):
    """benchmark index creation time followed by a lookup

    The default is to look `tip` up. Depending on the index implementation,
    the revision looked up can matters. For example, an implementation
    scanning the index will have a faster lookup time for `--rev tip` than for
    `--rev 0`. The number of looked up revisions and their order can also
    matters.

    Example of useful set to test:

    * tip
    * 0
    * -10:
    * :10
    * -10: + :10
    * :10: + -10:
    * -10000:
    * -10000: + 0

    It is not currently possible to check for lookup of a missing node. For
    deeper lookup benchmarking, checkout the `perfnodemap` command."""
    import mercurial.revlog

    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    mercurial.revlog._prereadsize = 2 ** 24  # disable lazy parser in old hg
    if opts[b'no_lookup']:
        if opts['rev']:
            raise error.Abort('--no-lookup and --rev are mutually exclusive')
        nodes = []
    elif not opts[b'rev']:
        nodes = [repo[b"tip"].node()]
    else:
        revs = scmutil.revrange(repo, opts[b'rev'])
        cl = repo.changelog
        nodes = [cl.node(r) for r in revs]

    unfi = repo.unfiltered()
    # find the filecache func directly
    # This avoid polluting the benchmark with the filecache logic
    makecl = unfi.__class__.changelog.func

    def setup():
        # probably not necessary, but for good measure
        clearchangelog(unfi)

    def d():
        cl = makecl(unfi)
        for n in nodes:
            cl.rev(n)

    timer(d, setup=setup)
    fm.end()


@command(
    b'perf::nodemap|perfnodemap',
    [
        (b'', b'rev', [], b'revision to be looked up (default tip)'),
        (b'', b'clear-caches', True, b'clear revlog cache between calls'),
    ]
    + formatteropts,
)
def perfnodemap(ui, repo, **opts):
    """benchmark the time necessary to look up revision from a cold nodemap

    Depending on the implementation, the amount and order of revision we look
    up can varies. Example of useful set to test:
    * tip
    * 0
    * -10:
    * :10
    * -10: + :10
    * :10: + -10:
    * -10000:
    * -10000: + 0

    The command currently focus on valid binary lookup. Benchmarking for
    hexlookup, prefix lookup and missing lookup would also be valuable.
    """
    import mercurial.revlog

    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    mercurial.revlog._prereadsize = 2 ** 24  # disable lazy parser in old hg

    unfi = repo.unfiltered()
    clearcaches = opts[b'clear_caches']
    # find the filecache func directly
    # This avoid polluting the benchmark with the filecache logic
    makecl = unfi.__class__.changelog.func
    if not opts[b'rev']:
        raise error.Abort(b'use --rev to specify revisions to look up')
    revs = scmutil.revrange(repo, opts[b'rev'])
    cl = repo.changelog
    nodes = [cl.node(r) for r in revs]

    # use a list to pass reference to a nodemap from one closure to the next
    nodeget = [None]

    def setnodeget():
        # probably not necessary, but for good measure
        clearchangelog(unfi)
        cl = makecl(unfi)
        if util.safehasattr(cl.index, 'get_rev'):
            nodeget[0] = cl.index.get_rev
        else:
            nodeget[0] = cl.nodemap.get

    def d():
        get = nodeget[0]
        for n in nodes:
            get(n)

    setup = None
    if clearcaches:

        def setup():
            setnodeget()

    else:
        setnodeget()
        d()  # prewarm the data structure
    timer(d, setup=setup)
    fm.end()


@command(b'perf::startup|perfstartup', formatteropts)
def perfstartup(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)

    def d():
        if os.name != 'nt':
            os.system(
                b"HGRCPATH= %s version -q > /dev/null" % fsencode(sys.argv[0])
            )
        else:
            os.environ['HGRCPATH'] = r' '
            os.system("%s version -q > NUL" % sys.argv[0])

    timer(d)
    fm.end()


@command(b'perf::parents|perfparents', formatteropts)
def perfparents(ui, repo, **opts):
    """benchmark the time necessary to fetch one changeset's parents.

    The fetch is done using the `node identifier`, traversing all object layers
    from the repository object. The first N revisions will be used for this
    benchmark. N is controlled by the ``perf.parentscount`` config option
    (default: 1000).
    """
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    # control the number of commits perfparents iterates over
    # experimental config: perf.parentscount
    count = getint(ui, b"perf", b"parentscount", 1000)
    if len(repo.changelog) < count:
        raise error.Abort(b"repo needs %d commits for this test" % count)
    repo = repo.unfiltered()
    nl = [repo.changelog.node(i) for i in _xrange(count)]

    def d():
        for n in nl:
            repo.changelog.parents(n)

    timer(d)
    fm.end()


@command(b'perf::ctxfiles|perfctxfiles', formatteropts)
def perfctxfiles(ui, repo, x, **opts):
    opts = _byteskwargs(opts)
    x = int(x)
    timer, fm = gettimer(ui, opts)

    def d():
        len(repo[x].files())

    timer(d)
    fm.end()


@command(b'perf::rawfiles|perfrawfiles', formatteropts)
def perfrawfiles(ui, repo, x, **opts):
    opts = _byteskwargs(opts)
    x = int(x)
    timer, fm = gettimer(ui, opts)
    cl = repo.changelog

    def d():
        len(cl.read(x)[3])

    timer(d)
    fm.end()


@command(b'perf::lookup|perflookup', formatteropts)
def perflookup(ui, repo, rev, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    timer(lambda: len(repo.lookup(rev)))
    fm.end()


@command(
    b'perf::linelogedits|perflinelogedits',
    [
        (b'n', b'edits', 10000, b'number of edits'),
        (b'', b'max-hunk-lines', 10, b'max lines in a hunk'),
    ],
    norepo=True,
)
def perflinelogedits(ui, **opts):
    from mercurial import linelog

    opts = _byteskwargs(opts)

    edits = opts[b'edits']
    maxhunklines = opts[b'max_hunk_lines']

    maxb1 = 100000
    random.seed(0)
    randint = random.randint
    currentlines = 0
    arglist = []
    for rev in _xrange(edits):
        a1 = randint(0, currentlines)
        a2 = randint(a1, min(currentlines, a1 + maxhunklines))
        b1 = randint(0, maxb1)
        b2 = randint(b1, b1 + maxhunklines)
        currentlines += (b2 - b1) - (a2 - a1)
        arglist.append((rev, a1, a2, b1, b2))

    def d():
        ll = linelog.linelog()
        for args in arglist:
            ll.replacelines(*args)

    timer, fm = gettimer(ui, opts)
    timer(d)
    fm.end()


@command(b'perf::revrange|perfrevrange', formatteropts)
def perfrevrange(ui, repo, *specs, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    revrange = scmutil.revrange
    timer(lambda: len(revrange(repo, specs)))
    fm.end()


@command(b'perf::nodelookup|perfnodelookup', formatteropts)
def perfnodelookup(ui, repo, rev, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    import mercurial.revlog

    mercurial.revlog._prereadsize = 2 ** 24  # disable lazy parser in old hg
    n = scmutil.revsingle(repo, rev).node()

    try:
        cl = revlog(getsvfs(repo), radix=b"00changelog")
    except TypeError:
        cl = revlog(getsvfs(repo), indexfile=b"00changelog.i")

    def d():
        cl.rev(n)
        clearcaches(cl)

    timer(d)
    fm.end()


@command(
    b'perf::log|perflog',
    [(b'', b'rename', False, b'ask log to follow renames')] + formatteropts,
)
def perflog(ui, repo, rev=None, **opts):
    opts = _byteskwargs(opts)
    if rev is None:
        rev = []
    timer, fm = gettimer(ui, opts)
    ui.pushbuffer()
    timer(
        lambda: commands.log(
            ui, repo, rev=rev, date=b'', user=b'', copies=opts.get(b'rename')
        )
    )
    ui.popbuffer()
    fm.end()


@command(b'perf::moonwalk|perfmoonwalk', formatteropts)
def perfmoonwalk(ui, repo, **opts):
    """benchmark walking the changelog backwards

    This also loads the changelog data for each revision in the changelog.
    """
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)

    def moonwalk():
        for i in repo.changelog.revs(start=(len(repo) - 1), stop=-1):
            ctx = repo[i]
            ctx.branch()  # read changelog data (in addition to the index)

    timer(moonwalk)
    fm.end()


@command(
    b'perf::templating|perftemplating',
    [
        (b'r', b'rev', [], b'revisions to run the template on'),
    ]
    + formatteropts,
)
def perftemplating(ui, repo, testedtemplate=None, **opts):
    """test the rendering time of a given template"""
    if makelogtemplater is None:
        raise error.Abort(
            b"perftemplating not available with this Mercurial",
            hint=b"use 4.3 or later",
        )

    opts = _byteskwargs(opts)

    nullui = ui.copy()
    nullui.fout = open(os.devnull, 'wb')
    nullui.disablepager()
    revs = opts.get(b'rev')
    if not revs:
        revs = [b'all()']
    revs = list(scmutil.revrange(repo, revs))

    defaulttemplate = (
        b'{date|shortdate} [{rev}:{node|short}]'
        b' {author|person}: {desc|firstline}\n'
    )
    if testedtemplate is None:
        testedtemplate = defaulttemplate
    displayer = makelogtemplater(nullui, repo, testedtemplate)

    def format():
        for r in revs:
            ctx = repo[r]
            displayer.show(ctx)
            displayer.flush(ctx)

    timer, fm = gettimer(ui, opts)
    timer(format)
    fm.end()


def _displaystats(ui, opts, entries, data):
    # use a second formatter because the data are quite different, not sure
    # how it flies with the templater.
    fm = ui.formatter(b'perf-stats', opts)
    for key, title in entries:
        values = data[key]
        nbvalues = len(data)
        values.sort()
        stats = {
            'key': key,
            'title': title,
            'nbitems': len(values),
            'min': values[0][0],
            '10%': values[(nbvalues * 10) // 100][0],
            '25%': values[(nbvalues * 25) // 100][0],
            '50%': values[(nbvalues * 50) // 100][0],
            '75%': values[(nbvalues * 75) // 100][0],
            '80%': values[(nbvalues * 80) // 100][0],
            '85%': values[(nbvalues * 85) // 100][0],
            '90%': values[(nbvalues * 90) // 100][0],
            '95%': values[(nbvalues * 95) // 100][0],
            '99%': values[(nbvalues * 99) // 100][0],
            'max': values[-1][0],
        }
        fm.startitem()
        fm.data(**stats)
        # make node pretty for the human output
        fm.plain('### %s (%d items)\n' % (title, len(values)))
        lines = [
            'min',
            '10%',
            '25%',
            '50%',
            '75%',
            '80%',
            '85%',
            '90%',
            '95%',
            '99%',
            'max',
        ]
        for l in lines:
            fm.plain('%s: %s\n' % (l, stats[l]))
    fm.end()


@command(
    b'perf::helper-mergecopies|perfhelper-mergecopies',
    formatteropts
    + [
        (b'r', b'revs', [], b'restrict search to these revisions'),
        (b'', b'timing', False, b'provides extra data (costly)'),
        (b'', b'stats', False, b'provides statistic about the measured data'),
    ],
)
def perfhelpermergecopies(ui, repo, revs=[], **opts):
    """find statistics about potential parameters for `perfmergecopies`

    This command find (base, p1, p2) triplet relevant for copytracing
    benchmarking in the context of a merge.  It reports values for some of the
    parameters that impact merge copy tracing time during merge.

    If `--timing` is set, rename detection is run and the associated timing
    will be reported. The extra details come at the cost of slower command
    execution.

    Since rename detection is only run once, other factors might easily
    affect the precision of the timing. However it should give a good
    approximation of which revision triplets are very costly.
    """
    opts = _byteskwargs(opts)
    fm = ui.formatter(b'perf', opts)
    dotiming = opts[b'timing']
    dostats = opts[b'stats']

    output_template = [
        ("base", "%(base)12s"),
        ("p1", "%(p1.node)12s"),
        ("p2", "%(p2.node)12s"),
        ("p1.nb-revs", "%(p1.nbrevs)12d"),
        ("p1.nb-files", "%(p1.nbmissingfiles)12d"),
        ("p1.renames", "%(p1.renamedfiles)12d"),
        ("p1.time", "%(p1.time)12.3f"),
        ("p2.nb-revs", "%(p2.nbrevs)12d"),
        ("p2.nb-files", "%(p2.nbmissingfiles)12d"),
        ("p2.renames", "%(p2.renamedfiles)12d"),
        ("p2.time", "%(p2.time)12.3f"),
        ("renames", "%(nbrenamedfiles)12d"),
        ("total.time", "%(time)12.3f"),
    ]
    if not dotiming:
        output_template = [
            i
            for i in output_template
            if not ('time' in i[0] or 'renames' in i[0])
        ]
    header_names = [h for (h, v) in output_template]
    output = ' '.join([v for (h, v) in output_template]) + '\n'
    header = ' '.join(['%12s'] * len(header_names)) + '\n'
    fm.plain(header % tuple(header_names))

    if not revs:
        revs = ['all()']
    revs = scmutil.revrange(repo, revs)

    if dostats:
        alldata = {
            'nbrevs': [],
            'nbmissingfiles': [],
        }
        if dotiming:
            alldata['parentnbrenames'] = []
            alldata['totalnbrenames'] = []
            alldata['parenttime'] = []
            alldata['totaltime'] = []

    roi = repo.revs('merge() and %ld', revs)
    for r in roi:
        ctx = repo[r]
        p1 = ctx.p1()
        p2 = ctx.p2()
        bases = repo.changelog._commonancestorsheads(p1.rev(), p2.rev())
        for b in bases:
            b = repo[b]
            p1missing = copies._computeforwardmissing(b, p1)
            p2missing = copies._computeforwardmissing(b, p2)
            data = {
                b'base': b.hex(),
                b'p1.node': p1.hex(),
                b'p1.nbrevs': len(repo.revs('only(%d, %d)', p1.rev(), b.rev())),
                b'p1.nbmissingfiles': len(p1missing),
                b'p2.node': p2.hex(),
                b'p2.nbrevs': len(repo.revs('only(%d, %d)', p2.rev(), b.rev())),
                b'p2.nbmissingfiles': len(p2missing),
            }
            if dostats:
                if p1missing:
                    alldata['nbrevs'].append(
                        (data['p1.nbrevs'], b.hex(), p1.hex())
                    )
                    alldata['nbmissingfiles'].append(
                        (data['p1.nbmissingfiles'], b.hex(), p1.hex())
                    )
                if p2missing:
                    alldata['nbrevs'].append(
                        (data['p2.nbrevs'], b.hex(), p2.hex())
                    )
                    alldata['nbmissingfiles'].append(
                        (data['p2.nbmissingfiles'], b.hex(), p2.hex())
                    )
            if dotiming:
                begin = util.timer()
                mergedata = copies.mergecopies(repo, p1, p2, b)
                end = util.timer()
                # not very stable timing since we did only one run
                data['time'] = end - begin
                # mergedata contains five dicts: "copy", "movewithdir",
                # "diverge", "renamedelete" and "dirmove".
                # The first 4 are about renamed file so lets count that.
                renames = len(mergedata[0])
                renames += len(mergedata[1])
                renames += len(mergedata[2])
                renames += len(mergedata[3])
                data['nbrenamedfiles'] = renames
                begin = util.timer()
                p1renames = copies.pathcopies(b, p1)
                end = util.timer()
                data['p1.time'] = end - begin
                begin = util.timer()
                p2renames = copies.pathcopies(b, p2)
                end = util.timer()
                data['p2.time'] = end - begin
                data['p1.renamedfiles'] = len(p1renames)
                data['p2.renamedfiles'] = len(p2renames)

                if dostats:
                    if p1missing:
                        alldata['parentnbrenames'].append(
                            (data['p1.renamedfiles'], b.hex(), p1.hex())
                        )
                        alldata['parenttime'].append(
                            (data['p1.time'], b.hex(), p1.hex())
                        )
                    if p2missing:
                        alldata['parentnbrenames'].append(
                            (data['p2.renamedfiles'], b.hex(), p2.hex())
                        )
                        alldata['parenttime'].append(
                            (data['p2.time'], b.hex(), p2.hex())
                        )
                    if p1missing or p2missing:
                        alldata['totalnbrenames'].append(
                            (
                                data['nbrenamedfiles'],
                                b.hex(),
                                p1.hex(),
                                p2.hex(),
                            )
                        )
                        alldata['totaltime'].append(
                            (data['time'], b.hex(), p1.hex(), p2.hex())
                        )
            fm.startitem()
            fm.data(**data)
            # make node pretty for the human output
            out = data.copy()
            out['base'] = fm.hexfunc(b.node())
            out['p1.node'] = fm.hexfunc(p1.node())
            out['p2.node'] = fm.hexfunc(p2.node())
            fm.plain(output % out)

    fm.end()
    if dostats:
        # use a second formatter because the data are quite different, not sure
        # how it flies with the templater.
        entries = [
            ('nbrevs', 'number of revision covered'),
            ('nbmissingfiles', 'number of missing files at head'),
        ]
        if dotiming:
            entries.append(
                ('parentnbrenames', 'rename from one parent to base')
            )
            entries.append(('totalnbrenames', 'total number of renames'))
            entries.append(('parenttime', 'time for one parent'))
            entries.append(('totaltime', 'time for both parents'))
        _displaystats(ui, opts, entries, alldata)


@command(
    b'perf::helper-pathcopies|perfhelper-pathcopies',
    formatteropts
    + [
        (b'r', b'revs', [], b'restrict search to these revisions'),
        (b'', b'timing', False, b'provides extra data (costly)'),
        (b'', b'stats', False, b'provides statistic about the measured data'),
    ],
)
def perfhelperpathcopies(ui, repo, revs=[], **opts):
    """find statistic about potential parameters for the `perftracecopies`

    This command find source-destination pair relevant for copytracing testing.
    It report value for some of the parameters that impact copy tracing time.

    If `--timing` is set, rename detection is run and the associated timing
    will be reported. The extra details comes at the cost of a slower command
    execution.

    Since the rename detection is only run once, other factors might easily
    affect the precision of the timing. However it should give a good
    approximation of which revision pairs are very costly.
    """
    opts = _byteskwargs(opts)
    fm = ui.formatter(b'perf', opts)
    dotiming = opts[b'timing']
    dostats = opts[b'stats']

    if dotiming:
        header = '%12s %12s %12s %12s %12s %12s\n'
        output = (
            "%(source)12s %(destination)12s "
            "%(nbrevs)12d %(nbmissingfiles)12d "
            "%(nbrenamedfiles)12d %(time)18.5f\n"
        )
        header_names = (
            "source",
            "destination",
            "nb-revs",
            "nb-files",
            "nb-renames",
            "time",
        )
        fm.plain(header % header_names)
    else:
        header = '%12s %12s %12s %12s\n'
        output = (
            "%(source)12s %(destination)12s "
            "%(nbrevs)12d %(nbmissingfiles)12d\n"
        )
        fm.plain(header % ("source", "destination", "nb-revs", "nb-files"))

    if not revs:
        revs = ['all()']
    revs = scmutil.revrange(repo, revs)

    if dostats:
        alldata = {
            'nbrevs': [],
            'nbmissingfiles': [],
        }
        if dotiming:
            alldata['nbrenames'] = []
            alldata['time'] = []

    roi = repo.revs('merge() and %ld', revs)
    for r in roi:
        ctx = repo[r]
        p1 = ctx.p1().rev()
        p2 = ctx.p2().rev()
        bases = repo.changelog._commonancestorsheads(p1, p2)
        for p in (p1, p2):
            for b in bases:
                base = repo[b]
                parent = repo[p]
                missing = copies._computeforwardmissing(base, parent)
                if not missing:
                    continue
                data = {
                    b'source': base.hex(),
                    b'destination': parent.hex(),
                    b'nbrevs': len(repo.revs('only(%d, %d)', p, b)),
                    b'nbmissingfiles': len(missing),
                }
                if dostats:
                    alldata['nbrevs'].append(
                        (
                            data['nbrevs'],
                            base.hex(),
                            parent.hex(),
                        )
                    )
                    alldata['nbmissingfiles'].append(
                        (
                            data['nbmissingfiles'],
                            base.hex(),
                            parent.hex(),
                        )
                    )
                if dotiming:
                    begin = util.timer()
                    renames = copies.pathcopies(base, parent)
                    end = util.timer()
                    # not very stable timing since we did only one run
                    data['time'] = end - begin
                    data['nbrenamedfiles'] = len(renames)
                    if dostats:
                        alldata['time'].append(
                            (
                                data['time'],
                                base.hex(),
                                parent.hex(),
                            )
                        )
                        alldata['nbrenames'].append(
                            (
                                data['nbrenamedfiles'],
                                base.hex(),
                                parent.hex(),
                            )
                        )
                fm.startitem()
                fm.data(**data)
                out = data.copy()
                out['source'] = fm.hexfunc(base.node())
                out['destination'] = fm.hexfunc(parent.node())
                fm.plain(output % out)

    fm.end()
    if dostats:
        entries = [
            ('nbrevs', 'number of revision covered'),
            ('nbmissingfiles', 'number of missing files at head'),
        ]
        if dotiming:
            entries.append(('nbrenames', 'renamed files'))
            entries.append(('time', 'time'))
        _displaystats(ui, opts, entries, alldata)


@command(b'perf::cca|perfcca', formatteropts)
def perfcca(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    timer(lambda: scmutil.casecollisionauditor(ui, False, repo.dirstate))
    fm.end()


@command(b'perf::fncacheload|perffncacheload', formatteropts)
def perffncacheload(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    s = repo.store

    def d():
        s.fncache._load()

    timer(d)
    fm.end()


@command(b'perf::fncachewrite|perffncachewrite', formatteropts)
def perffncachewrite(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    s = repo.store
    lock = repo.lock()
    s.fncache._load()
    tr = repo.transaction(b'perffncachewrite')
    tr.addbackup(b'fncache')

    def d():
        s.fncache._dirty = True
        s.fncache.write(tr)

    timer(d)
    tr.close()
    lock.release()
    fm.end()


@command(b'perf::fncacheencode|perffncacheencode', formatteropts)
def perffncacheencode(ui, repo, **opts):
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    s = repo.store
    s.fncache._load()

    def d():
        for p in s.fncache.entries:
            s.encode(p)

    timer(d)
    fm.end()


def _bdiffworker(q, blocks, xdiff, ready, done):
    while not done.is_set():
        pair = q.get()
        while pair is not None:
            if xdiff:
                mdiff.bdiff.xdiffblocks(*pair)
            elif blocks:
                mdiff.bdiff.blocks(*pair)
            else:
                mdiff.textdiff(*pair)
            q.task_done()
            pair = q.get()
        q.task_done()  # for the None one
        with ready:
            ready.wait()


def _manifestrevision(repo, mnode):
    ml = repo.manifestlog

    if util.safehasattr(ml, b'getstorage'):
        store = ml.getstorage(b'')
    else:
        store = ml._revlog

    return store.revision(mnode)


@command(
    b'perf::bdiff|perfbdiff',
    revlogopts
    + formatteropts
    + [
        (
            b'',
            b'count',
            1,
            b'number of revisions to test (when using --startrev)',
        ),
        (b'', b'alldata', False, b'test bdiffs for all associated revisions'),
        (b'', b'threads', 0, b'number of thread to use (disable with 0)'),
        (b'', b'blocks', False, b'test computing diffs into blocks'),
        (b'', b'xdiff', False, b'use xdiff algorithm'),
    ],
    b'-c|-m|FILE REV',
)
def perfbdiff(ui, repo, file_, rev=None, count=None, threads=0, **opts):
    """benchmark a bdiff between revisions

    By default, benchmark a bdiff between its delta parent and itself.

    With ``--count``, benchmark bdiffs between delta parents and self for N
    revisions starting at the specified revision.

    With ``--alldata``, assume the requested revision is a changeset and
    measure bdiffs for all changes related to that changeset (manifest
    and filelogs).
    """
    opts = _byteskwargs(opts)

    if opts[b'xdiff'] and not opts[b'blocks']:
        raise error.CommandError(b'perfbdiff', b'--xdiff requires --blocks')

    if opts[b'alldata']:
        opts[b'changelog'] = True

    if opts.get(b'changelog') or opts.get(b'manifest'):
        file_, rev = None, file_
    elif rev is None:
        raise error.CommandError(b'perfbdiff', b'invalid arguments')

    blocks = opts[b'blocks']
    xdiff = opts[b'xdiff']
    textpairs = []

    r = cmdutil.openrevlog(repo, b'perfbdiff', file_, opts)

    startrev = r.rev(r.lookup(rev))
    for rev in range(startrev, min(startrev + count, len(r) - 1)):
        if opts[b'alldata']:
            # Load revisions associated with changeset.
            ctx = repo[rev]
            mtext = _manifestrevision(repo, ctx.manifestnode())
            for pctx in ctx.parents():
                pman = _manifestrevision(repo, pctx.manifestnode())
                textpairs.append((pman, mtext))

            # Load filelog revisions by iterating manifest delta.
            man = ctx.manifest()
            pman = ctx.p1().manifest()
            for filename, change in pman.diff(man).items():
                fctx = repo.file(filename)
                f1 = fctx.revision(change[0][0] or -1)
                f2 = fctx.revision(change[1][0] or -1)
                textpairs.append((f1, f2))
        else:
            dp = r.deltaparent(rev)
            textpairs.append((r.revision(dp), r.revision(rev)))

    withthreads = threads > 0
    if not withthreads:

        def d():
            for pair in textpairs:
                if xdiff:
                    mdiff.bdiff.xdiffblocks(*pair)
                elif blocks:
                    mdiff.bdiff.blocks(*pair)
                else:
                    mdiff.textdiff(*pair)

    else:
        q = queue()
        for i in _xrange(threads):
            q.put(None)
        ready = threading.Condition()
        done = threading.Event()
        for i in _xrange(threads):
            threading.Thread(
                target=_bdiffworker, args=(q, blocks, xdiff, ready, done)
            ).start()
        q.join()

        def d():
            for pair in textpairs:
                q.put(pair)
            for i in _xrange(threads):
                q.put(None)
            with ready:
                ready.notify_all()
            q.join()

    timer, fm = gettimer(ui, opts)
    timer(d)
    fm.end()

    if withthreads:
        done.set()
        for i in _xrange(threads):
            q.put(None)
        with ready:
            ready.notify_all()


@command(
    b'perf::unbundle',
    formatteropts,
    b'BUNDLE_FILE',
)
def perf_unbundle(ui, repo, fname, **opts):
    """benchmark application of a bundle in a repository.

    This does not include the final transaction processing"""

    from mercurial import exchange
    from mercurial import bundle2
    from mercurial import transaction

    opts = _byteskwargs(opts)

    ###  some compatibility hotfix
    #
    # the data attribute is dropped in 63edc384d3b7 a changeset introducing a
    # critical regression that break transaction rollback for files that are
    # de-inlined.
    method = transaction.transaction._addentry
    pre_63edc384d3b7 = "data" in getargspec(method).args
    # the `detailed_exit_code` attribute is introduced in 33c0c25d0b0f
    # a changeset that is a close descendant of 18415fc918a1, the changeset
    # that conclude the fix run for the bug introduced in 63edc384d3b7.
    args = getargspec(error.Abort.__init__).args
    post_18415fc918a1 = "detailed_exit_code" in args

    old_max_inline = None
    try:
        if not (pre_63edc384d3b7 or post_18415fc918a1):
            # disable inlining
            old_max_inline = mercurial.revlog._maxinline
            # large enough to never happen
            mercurial.revlog._maxinline = 2 ** 50

        with repo.lock():
            bundle = [None, None]
            orig_quiet = repo.ui.quiet
            try:
                repo.ui.quiet = True
                with open(fname, mode="rb") as f:

                    def noop_report(*args, **kwargs):
                        pass

                    def setup():
                        gen, tr = bundle
                        if tr is not None:
                            tr.abort()
                        bundle[:] = [None, None]
                        f.seek(0)
                        bundle[0] = exchange.readbundle(ui, f, fname)
                        bundle[1] = repo.transaction(b'perf::unbundle')
                        # silence the transaction
                        bundle[1]._report = noop_report

                    def apply():
                        gen, tr = bundle
                        bundle2.applybundle(
                            repo,
                            gen,
                            tr,
                            source=b'perf::unbundle',
                            url=fname,
                        )

                    timer, fm = gettimer(ui, opts)
                    timer(apply, setup=setup)
                    fm.end()
            finally:
                repo.ui.quiet == orig_quiet
                gen, tr = bundle
                if tr is not None:
                    tr.abort()
    finally:
        if old_max_inline is not None:
            mercurial.revlog._maxinline = old_max_inline


@command(
    b'perf::unidiff|perfunidiff',
    revlogopts
    + formatteropts
    + [
        (
            b'',
            b'count',
            1,
            b'number of revisions to test (when using --startrev)',
        ),
        (b'', b'alldata', False, b'test unidiffs for all associated revisions'),
    ],
    b'-c|-m|FILE REV',
)
def perfunidiff(ui, repo, file_, rev=None, count=None, **opts):
    """benchmark a unified diff between revisions

    This doesn't include any copy tracing - it's just a unified diff
    of the texts.

    By default, benchmark a diff between its delta parent and itself.

    With ``--count``, benchmark diffs between delta parents and self for N
    revisions starting at the specified revision.

    With ``--alldata``, assume the requested revision is a changeset and
    measure diffs for all changes related to that changeset (manifest
    and filelogs).
    """
    opts = _byteskwargs(opts)
    if opts[b'alldata']:
        opts[b'changelog'] = True

    if opts.get(b'changelog') or opts.get(b'manifest'):
        file_, rev = None, file_
    elif rev is None:
        raise error.CommandError(b'perfunidiff', b'invalid arguments')

    textpairs = []

    r = cmdutil.openrevlog(repo, b'perfunidiff', file_, opts)

    startrev = r.rev(r.lookup(rev))
    for rev in range(startrev, min(startrev + count, len(r) - 1)):
        if opts[b'alldata']:
            # Load revisions associated with changeset.
            ctx = repo[rev]
            mtext = _manifestrevision(repo, ctx.manifestnode())
            for pctx in ctx.parents():
                pman = _manifestrevision(repo, pctx.manifestnode())
                textpairs.append((pman, mtext))

            # Load filelog revisions by iterating manifest delta.
            man = ctx.manifest()
            pman = ctx.p1().manifest()
            for filename, change in pman.diff(man).items():
                fctx = repo.file(filename)
                f1 = fctx.revision(change[0][0] or -1)
                f2 = fctx.revision(change[1][0] or -1)
                textpairs.append((f1, f2))
        else:
            dp = r.deltaparent(rev)
            textpairs.append((r.revision(dp), r.revision(rev)))

    def d():
        for left, right in textpairs:
            # The date strings don't matter, so we pass empty strings.
            headerlines, hunks = mdiff.unidiff(
                left, b'', right, b'', b'left', b'right', binary=False
            )
            # consume iterators in roughly the way patch.py does
            b'\n'.join(headerlines)
            b''.join(sum((list(hlines) for hrange, hlines in hunks), []))

    timer, fm = gettimer(ui, opts)
    timer(d)
    fm.end()


@command(b'perf::diffwd|perfdiffwd', formatteropts)
def perfdiffwd(ui, repo, **opts):
    """Profile diff of working directory changes"""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    options = {
        'w': 'ignore_all_space',
        'b': 'ignore_space_change',
        'B': 'ignore_blank_lines',
    }

    for diffopt in ('', 'w', 'b', 'B', 'wB'):
        opts = {options[c]: b'1' for c in diffopt}

        def d():
            ui.pushbuffer()
            commands.diff(ui, repo, **opts)
            ui.popbuffer()

        diffopt = diffopt.encode('ascii')
        title = b'diffopts: %s' % (diffopt and (b'-' + diffopt) or b'none')
        timer(d, title=title)
    fm.end()


@command(
    b'perf::revlogindex|perfrevlogindex',
    revlogopts + formatteropts,
    b'-c|-m|FILE',
)
def perfrevlogindex(ui, repo, file_=None, **opts):
    """Benchmark operations against a revlog index.

    This tests constructing a revlog instance, reading index data,
    parsing index data, and performing various operations related to
    index data.
    """

    opts = _byteskwargs(opts)

    rl = cmdutil.openrevlog(repo, b'perfrevlogindex', file_, opts)

    opener = getattr(rl, 'opener')  # trick linter
    # compat with hg <= 5.8
    radix = getattr(rl, 'radix', None)
    indexfile = getattr(rl, '_indexfile', None)
    if indexfile is None:
        # compatibility with <= hg-5.8
        indexfile = getattr(rl, 'indexfile')
    data = opener.read(indexfile)

    header = struct.unpack(b'>I', data[0:4])[0]
    version = header & 0xFFFF
    if version == 1:
        inline = header & (1 << 16)
    else:
        raise error.Abort(b'unsupported revlog version: %d' % version)

    parse_index_v1 = getattr(mercurial.revlog, 'parse_index_v1', None)
    if parse_index_v1 is None:
        parse_index_v1 = mercurial.revlog.revlogio().parseindex

    rllen = len(rl)

    node0 = rl.node(0)
    node25 = rl.node(rllen // 4)
    node50 = rl.node(rllen // 2)
    node75 = rl.node(rllen // 4 * 3)
    node100 = rl.node(rllen - 1)

    allrevs = range(rllen)
    allrevsrev = list(reversed(allrevs))
    allnodes = [rl.node(rev) for rev in range(rllen)]
    allnodesrev = list(reversed(allnodes))

    def constructor():
        if radix is not None:
            revlog(opener, radix=radix)
        else:
            # hg <= 5.8
            revlog(opener, indexfile=indexfile)

    def read():
        with opener(indexfile) as fh:
            fh.read()

    def parseindex():
        parse_index_v1(data, inline)

    def getentry(revornode):
        index = parse_index_v1(data, inline)[0]
        index[revornode]

    def getentries(revs, count=1):
        index = parse_index_v1(data, inline)[0]

        for i in range(count):
            for rev in revs:
                index[rev]

    def resolvenode(node):
        index = parse_index_v1(data, inline)[0]
        rev = getattr(index, 'rev', None)
        if rev is None:
            nodemap = getattr(parse_index_v1(data, inline)[0], 'nodemap', None)
            # This only works for the C code.
            if nodemap is None:
                return
            rev = nodemap.__getitem__

        try:
            rev(node)
        except error.RevlogError:
            pass

    def resolvenodes(nodes, count=1):
        index = parse_index_v1(data, inline)[0]
        rev = getattr(index, 'rev', None)
        if rev is None:
            nodemap = getattr(parse_index_v1(data, inline)[0], 'nodemap', None)
            # This only works for the C code.
            if nodemap is None:
                return
            rev = nodemap.__getitem__

        for i in range(count):
            for node in nodes:
                try:
                    rev(node)
                except error.RevlogError:
                    pass

    benches = [
        (constructor, b'revlog constructor'),
        (read, b'read'),
        (parseindex, b'create index object'),
        (lambda: getentry(0), b'retrieve index entry for rev 0'),
        (lambda: resolvenode(b'a' * 20), b'look up missing node'),
        (lambda: resolvenode(node0), b'look up node at rev 0'),
        (lambda: resolvenode(node25), b'look up node at 1/4 len'),
        (lambda: resolvenode(node50), b'look up node at 1/2 len'),
        (lambda: resolvenode(node75), b'look up node at 3/4 len'),
        (lambda: resolvenode(node100), b'look up node at tip'),
        # 2x variation is to measure caching impact.
        (lambda: resolvenodes(allnodes), b'look up all nodes (forward)'),
        (lambda: resolvenodes(allnodes, 2), b'look up all nodes 2x (forward)'),
        (lambda: resolvenodes(allnodesrev), b'look up all nodes (reverse)'),
        (
            lambda: resolvenodes(allnodesrev, 2),
            b'look up all nodes 2x (reverse)',
        ),
        (lambda: getentries(allrevs), b'retrieve all index entries (forward)'),
        (
            lambda: getentries(allrevs, 2),
            b'retrieve all index entries 2x (forward)',
        ),
        (
            lambda: getentries(allrevsrev),
            b'retrieve all index entries (reverse)',
        ),
        (
            lambda: getentries(allrevsrev, 2),
            b'retrieve all index entries 2x (reverse)',
        ),
    ]

    for fn, title in benches:
        timer, fm = gettimer(ui, opts)
        timer(fn, title=title)
        fm.end()


@command(
    b'perf::revlogrevisions|perfrevlogrevisions',
    revlogopts
    + formatteropts
    + [
        (b'd', b'dist', 100, b'distance between the revisions'),
        (b's', b'startrev', 0, b'revision to start reading at'),
        (b'', b'reverse', False, b'read in reverse'),
    ],
    b'-c|-m|FILE',
)
def perfrevlogrevisions(
    ui, repo, file_=None, startrev=0, reverse=False, **opts
):
    """Benchmark reading a series of revisions from a revlog.

    By default, we read every ``-d/--dist`` revision from 0 to tip of
    the specified revlog.

    The start revision can be defined via ``-s/--startrev``.
    """
    opts = _byteskwargs(opts)

    rl = cmdutil.openrevlog(repo, b'perfrevlogrevisions', file_, opts)
    rllen = getlen(ui)(rl)

    if startrev < 0:
        startrev = rllen + startrev

    def d():
        rl.clearcaches()

        beginrev = startrev
        endrev = rllen
        dist = opts[b'dist']

        if reverse:
            beginrev, endrev = endrev - 1, beginrev - 1
            dist = -1 * dist

        for x in _xrange(beginrev, endrev, dist):
            # Old revisions don't support passing int.
            n = rl.node(x)
            rl.revision(n)

    timer, fm = gettimer(ui, opts)
    timer(d)
    fm.end()


@command(
    b'perf::revlogwrite|perfrevlogwrite',
    revlogopts
    + formatteropts
    + [
        (b's', b'startrev', 1000, b'revision to start writing at'),
        (b'', b'stoprev', -1, b'last revision to write'),
        (b'', b'count', 3, b'number of passes to perform'),
        (b'', b'details', False, b'print timing for every revisions tested'),
        (b'', b'source', b'full', b'the kind of data feed in the revlog'),
        (b'', b'lazydeltabase', True, b'try the provided delta first'),
        (b'', b'clear-caches', True, b'clear revlog cache between calls'),
    ],
    b'-c|-m|FILE',
)
def perfrevlogwrite(ui, repo, file_=None, startrev=1000, stoprev=-1, **opts):
    """Benchmark writing a series of revisions to a revlog.

    Possible source values are:
    * `full`: add from a full text (default).
    * `parent-1`: add from a delta to the first parent
    * `parent-2`: add from a delta to the second parent if it exists
                  (use a delta from the first parent otherwise)
    * `parent-smallest`: add from the smallest delta (either p1 or p2)
    * `storage`: add from the existing precomputed deltas

    Note: This performance command measures performance in a custom way. As a
    result some of the global configuration of the 'perf' command does not
    apply to it:

    * ``pre-run``: disabled

    * ``profile-benchmark``: disabled

    * ``run-limits``: disabled use --count instead
    """
    opts = _byteskwargs(opts)

    rl = cmdutil.openrevlog(repo, b'perfrevlogwrite', file_, opts)
    rllen = getlen(ui)(rl)
    if startrev < 0:
        startrev = rllen + startrev
    if stoprev < 0:
        stoprev = rllen + stoprev

    lazydeltabase = opts['lazydeltabase']
    source = opts['source']
    clearcaches = opts['clear_caches']
    validsource = (
        b'full',
        b'parent-1',
        b'parent-2',
        b'parent-smallest',
        b'storage',
    )
    if source not in validsource:
        raise error.Abort('invalid source type: %s' % source)

    ### actually gather results
    count = opts['count']
    if count <= 0:
        raise error.Abort('invalide run count: %d' % count)
    allresults = []
    for c in range(count):
        timing = _timeonewrite(
            ui,
            rl,
            source,
            startrev,
            stoprev,
            c + 1,
            lazydeltabase=lazydeltabase,
            clearcaches=clearcaches,
        )
        allresults.append(timing)

    ### consolidate the results in a single list
    results = []
    for idx, (rev, t) in enumerate(allresults[0]):
        ts = [t]
        for other in allresults[1:]:
            orev, ot = other[idx]
            assert orev == rev
            ts.append(ot)
        results.append((rev, ts))
    resultcount = len(results)

    ### Compute and display relevant statistics

    # get a formatter
    fm = ui.formatter(b'perf', opts)
    displayall = ui.configbool(b"perf", b"all-timing", False)

    # print individual details if requested
    if opts['details']:
        for idx, item in enumerate(results, 1):
            rev, data = item
            title = 'revisions #%d of %d, rev %d' % (idx, resultcount, rev)
            formatone(fm, data, title=title, displayall=displayall)

    # sorts results by median time
    results.sort(key=lambda x: sorted(x[1])[len(x[1]) // 2])
    # list of (name, index) to display)
    relevants = [
        ("min", 0),
        ("10%", resultcount * 10 // 100),
        ("25%", resultcount * 25 // 100),
        ("50%", resultcount * 70 // 100),
        ("75%", resultcount * 75 // 100),
        ("90%", resultcount * 90 // 100),
        ("95%", resultcount * 95 // 100),
        ("99%", resultcount * 99 // 100),
        ("99.9%", resultcount * 999 // 1000),
        ("99.99%", resultcount * 9999 // 10000),
        ("99.999%", resultcount * 99999 // 100000),
        ("max", -1),
    ]
    if not ui.quiet:
        for name, idx in relevants:
            data = results[idx]
            title = '%s of %d, rev %d' % (name, resultcount, data[0])
            formatone(fm, data[1], title=title, displayall=displayall)

    # XXX summing that many float will not be very precise, we ignore this fact
    # for now
    totaltime = []
    for item in allresults:
        totaltime.append(
            (
                sum(x[1][0] for x in item),
                sum(x[1][1] for x in item),
                sum(x[1][2] for x in item),
            )
        )
    formatone(
        fm,
        totaltime,
        title="total time (%d revs)" % resultcount,
        displayall=displayall,
    )
    fm.end()


class _faketr:
    def add(s, x, y, z=None):
        return None


def _timeonewrite(
    ui,
    orig,
    source,
    startrev,
    stoprev,
    runidx=None,
    lazydeltabase=True,
    clearcaches=True,
):
    timings = []
    tr = _faketr()
    with _temprevlog(ui, orig, startrev) as dest:
        dest._lazydeltabase = lazydeltabase
        revs = list(orig.revs(startrev, stoprev))
        total = len(revs)
        topic = 'adding'
        if runidx is not None:
            topic += ' (run #%d)' % runidx
        # Support both old and new progress API
        if util.safehasattr(ui, 'makeprogress'):
            progress = ui.makeprogress(topic, unit='revs', total=total)

            def updateprogress(pos):
                progress.update(pos)

            def completeprogress():
                progress.complete()

        else:

            def updateprogress(pos):
                ui.progress(topic, pos, unit='revs', total=total)

            def completeprogress():
                ui.progress(topic, None, unit='revs', total=total)

        for idx, rev in enumerate(revs):
            updateprogress(idx)
            addargs, addkwargs = _getrevisionseed(orig, rev, tr, source)
            if clearcaches:
                dest.index.clearcaches()
                dest.clearcaches()
            with timeone() as r:
                dest.addrawrevision(*addargs, **addkwargs)
            timings.append((rev, r[0]))
        updateprogress(total)
        completeprogress()
    return timings


def _getrevisionseed(orig, rev, tr, source):
    from mercurial.node import nullid

    linkrev = orig.linkrev(rev)
    node = orig.node(rev)
    p1, p2 = orig.parents(node)
    flags = orig.flags(rev)
    cachedelta = None
    text = None

    if source == b'full':
        text = orig.revision(rev)
    elif source == b'parent-1':
        baserev = orig.rev(p1)
        cachedelta = (baserev, orig.revdiff(p1, rev))
    elif source == b'parent-2':
        parent = p2
        if p2 == nullid:
            parent = p1
        baserev = orig.rev(parent)
        cachedelta = (baserev, orig.revdiff(parent, rev))
    elif source == b'parent-smallest':
        p1diff = orig.revdiff(p1, rev)
        parent = p1
        diff = p1diff
        if p2 != nullid:
            p2diff = orig.revdiff(p2, rev)
            if len(p1diff) > len(p2diff):
                parent = p2
                diff = p2diff
        baserev = orig.rev(parent)
        cachedelta = (baserev, diff)
    elif source == b'storage':
        baserev = orig.deltaparent(rev)
        cachedelta = (baserev, orig.revdiff(orig.node(baserev), rev))

    return (
        (text, tr, linkrev, p1, p2),
        {'node': node, 'flags': flags, 'cachedelta': cachedelta},
    )


@contextlib.contextmanager
def _temprevlog(ui, orig, truncaterev):
    from mercurial import vfs as vfsmod

    if orig._inline:
        raise error.Abort('not supporting inline revlog (yet)')
    revlogkwargs = {}
    k = 'upperboundcomp'
    if util.safehasattr(orig, k):
        revlogkwargs[k] = getattr(orig, k)

    indexfile = getattr(orig, '_indexfile', None)
    if indexfile is None:
        # compatibility with <= hg-5.8
        indexfile = getattr(orig, 'indexfile')
    origindexpath = orig.opener.join(indexfile)

    datafile = getattr(orig, '_datafile', getattr(orig, 'datafile'))
    origdatapath = orig.opener.join(datafile)
    radix = b'revlog'
    indexname = b'revlog.i'
    dataname = b'revlog.d'

    tmpdir = tempfile.mkdtemp(prefix='tmp-hgperf-')
    try:
        # copy the data file in a temporary directory
        ui.debug('copying data in %s\n' % tmpdir)
        destindexpath = os.path.join(tmpdir, 'revlog.i')
        destdatapath = os.path.join(tmpdir, 'revlog.d')
        shutil.copyfile(origindexpath, destindexpath)
        shutil.copyfile(origdatapath, destdatapath)

        # remove the data we want to add again
        ui.debug('truncating data to be rewritten\n')
        with open(destindexpath, 'ab') as index:
            index.seek(0)
            index.truncate(truncaterev * orig._io.size)
        with open(destdatapath, 'ab') as data:
            data.seek(0)
            data.truncate(orig.start(truncaterev))

        # instantiate a new revlog from the temporary copy
        ui.debug('truncating adding to be rewritten\n')
        vfs = vfsmod.vfs(tmpdir)
        vfs.options = getattr(orig.opener, 'options', None)

        try:
            dest = revlog(vfs, radix=radix, **revlogkwargs)
        except TypeError:
            dest = revlog(
                vfs, indexfile=indexname, datafile=dataname, **revlogkwargs
            )
        if dest._inline:
            raise error.Abort('not supporting inline revlog (yet)')
        # make sure internals are initialized
        dest.revision(len(dest) - 1)
        yield dest
        del dest, vfs
    finally:
        shutil.rmtree(tmpdir, True)


@command(
    b'perf::revlogchunks|perfrevlogchunks',
    revlogopts
    + formatteropts
    + [
        (b'e', b'engines', b'', b'compression engines to use'),
        (b's', b'startrev', 0, b'revision to start at'),
    ],
    b'-c|-m|FILE',
)
def perfrevlogchunks(ui, repo, file_=None, engines=None, startrev=0, **opts):
    """Benchmark operations on revlog chunks.

    Logically, each revlog is a collection of fulltext revisions. However,
    stored within each revlog are "chunks" of possibly compressed data. This
    data needs to be read and decompressed or compressed and written.

    This command measures the time it takes to read+decompress and recompress
    chunks in a revlog. It effectively isolates I/O and compression performance.
    For measurements of higher-level operations like resolving revisions,
    see ``perfrevlogrevisions`` and ``perfrevlogrevision``.
    """
    opts = _byteskwargs(opts)

    rl = cmdutil.openrevlog(repo, b'perfrevlogchunks', file_, opts)

    # _chunkraw was renamed to _getsegmentforrevs.
    try:
        segmentforrevs = rl._getsegmentforrevs
    except AttributeError:
        segmentforrevs = rl._chunkraw

    # Verify engines argument.
    if engines:
        engines = {e.strip() for e in engines.split(b',')}
        for engine in engines:
            try:
                util.compressionengines[engine]
            except KeyError:
                raise error.Abort(b'unknown compression engine: %s' % engine)
    else:
        engines = []
        for e in util.compengines:
            engine = util.compengines[e]
            try:
                if engine.available():
                    engine.revlogcompressor().compress(b'dummy')
                    engines.append(e)
            except NotImplementedError:
                pass

    revs = list(rl.revs(startrev, len(rl) - 1))

    def rlfh(rl):
        if rl._inline:
            indexfile = getattr(rl, '_indexfile', None)
            if indexfile is None:
                # compatibility with <= hg-5.8
                indexfile = getattr(rl, 'indexfile')
            return getsvfs(repo)(indexfile)
        else:
            datafile = getattr(rl, 'datafile', getattr(rl, 'datafile'))
            return getsvfs(repo)(datafile)

    def doread():
        rl.clearcaches()
        for rev in revs:
            segmentforrevs(rev, rev)

    def doreadcachedfh():
        rl.clearcaches()
        fh = rlfh(rl)
        for rev in revs:
            segmentforrevs(rev, rev, df=fh)

    def doreadbatch():
        rl.clearcaches()
        segmentforrevs(revs[0], revs[-1])

    def doreadbatchcachedfh():
        rl.clearcaches()
        fh = rlfh(rl)
        segmentforrevs(revs[0], revs[-1], df=fh)

    def dochunk():
        rl.clearcaches()
        fh = rlfh(rl)
        for rev in revs:
            rl._chunk(rev, df=fh)

    chunks = [None]

    def dochunkbatch():
        rl.clearcaches()
        fh = rlfh(rl)
        # Save chunks as a side-effect.
        chunks[0] = rl._chunks(revs, df=fh)

    def docompress(compressor):
        rl.clearcaches()

        try:
            # Swap in the requested compression engine.
            oldcompressor = rl._compressor
            rl._compressor = compressor
            for chunk in chunks[0]:
                rl.compress(chunk)
        finally:
            rl._compressor = oldcompressor

    benches = [
        (lambda: doread(), b'read'),
        (lambda: doreadcachedfh(), b'read w/ reused fd'),
        (lambda: doreadbatch(), b'read batch'),
        (lambda: doreadbatchcachedfh(), b'read batch w/ reused fd'),
        (lambda: dochunk(), b'chunk'),
        (lambda: dochunkbatch(), b'chunk batch'),
    ]

    for engine in sorted(engines):
        compressor = util.compengines[engine].revlogcompressor()
        benches.append(
            (
                functools.partial(docompress, compressor),
                b'compress w/ %s' % engine,
            )
        )

    for fn, title in benches:
        timer, fm = gettimer(ui, opts)
        timer(fn, title=title)
        fm.end()


@command(
    b'perf::revlogrevision|perfrevlogrevision',
    revlogopts
    + formatteropts
    + [(b'', b'cache', False, b'use caches instead of clearing')],
    b'-c|-m|FILE REV',
)
def perfrevlogrevision(ui, repo, file_, rev=None, cache=None, **opts):
    """Benchmark obtaining a revlog revision.

    Obtaining a revlog revision consists of roughly the following steps:

    1. Compute the delta chain
    2. Slice the delta chain if applicable
    3. Obtain the raw chunks for that delta chain
    4. Decompress each raw chunk
    5. Apply binary patches to obtain fulltext
    6. Verify hash of fulltext

    This command measures the time spent in each of these phases.
    """
    opts = _byteskwargs(opts)

    if opts.get(b'changelog') or opts.get(b'manifest'):
        file_, rev = None, file_
    elif rev is None:
        raise error.CommandError(b'perfrevlogrevision', b'invalid arguments')

    r = cmdutil.openrevlog(repo, b'perfrevlogrevision', file_, opts)

    # _chunkraw was renamed to _getsegmentforrevs.
    try:
        segmentforrevs = r._getsegmentforrevs
    except AttributeError:
        segmentforrevs = r._chunkraw

    node = r.lookup(rev)
    rev = r.rev(node)

    def getrawchunks(data, chain):
        start = r.start
        length = r.length
        inline = r._inline
        try:
            iosize = r.index.entry_size
        except AttributeError:
            iosize = r._io.size
        buffer = util.buffer

        chunks = []
        ladd = chunks.append
        for idx, item in enumerate(chain):
            offset = start(item[0])
            bits = data[idx]
            for rev in item:
                chunkstart = start(rev)
                if inline:
                    chunkstart += (rev + 1) * iosize
                chunklength = length(rev)
                ladd(buffer(bits, chunkstart - offset, chunklength))

        return chunks

    def dodeltachain(rev):
        if not cache:
            r.clearcaches()
        r._deltachain(rev)

    def doread(chain):
        if not cache:
            r.clearcaches()
        for item in slicedchain:
            segmentforrevs(item[0], item[-1])

    def doslice(r, chain, size):
        for s in slicechunk(r, chain, targetsize=size):
            pass

    def dorawchunks(data, chain):
        if not cache:
            r.clearcaches()
        getrawchunks(data, chain)

    def dodecompress(chunks):
        decomp = r.decompress
        for chunk in chunks:
            decomp(chunk)

    def dopatch(text, bins):
        if not cache:
            r.clearcaches()
        mdiff.patches(text, bins)

    def dohash(text):
        if not cache:
            r.clearcaches()
        r.checkhash(text, node, rev=rev)

    def dorevision():
        if not cache:
            r.clearcaches()
        r.revision(node)

    try:
        from mercurial.revlogutils.deltas import slicechunk
    except ImportError:
        slicechunk = getattr(revlog, '_slicechunk', None)

    size = r.length(rev)
    chain = r._deltachain(rev)[0]
    if not getattr(r, '_withsparseread', False):
        slicedchain = (chain,)
    else:
        slicedchain = tuple(slicechunk(r, chain, targetsize=size))
    data = [segmentforrevs(seg[0], seg[-1])[1] for seg in slicedchain]
    rawchunks = getrawchunks(data, slicedchain)
    bins = r._chunks(chain)
    text = bytes(bins[0])
    bins = bins[1:]
    text = mdiff.patches(text, bins)

    benches = [
        (lambda: dorevision(), b'full'),
        (lambda: dodeltachain(rev), b'deltachain'),
        (lambda: doread(chain), b'read'),
    ]

    if getattr(r, '_withsparseread', False):
        slicing = (lambda: doslice(r, chain, size), b'slice-sparse-chain')
        benches.append(slicing)

    benches.extend(
        [
            (lambda: dorawchunks(data, slicedchain), b'rawchunks'),
            (lambda: dodecompress(rawchunks), b'decompress'),
            (lambda: dopatch(text, bins), b'patch'),
            (lambda: dohash(text), b'hash'),
        ]
    )

    timer, fm = gettimer(ui, opts)
    for fn, title in benches:
        timer(fn, title=title)
    fm.end()


@command(
    b'perf::revset|perfrevset',
    [
        (b'C', b'clear', False, b'clear volatile cache between each call.'),
        (b'', b'contexts', False, b'obtain changectx for each revision'),
    ]
    + formatteropts,
    b"REVSET",
)
def perfrevset(ui, repo, expr, clear=False, contexts=False, **opts):
    """benchmark the execution time of a revset

    Use the --clean option if need to evaluate the impact of build volatile
    revisions set cache on the revset execution. Volatile cache hold filtered
    and obsolete related cache."""
    opts = _byteskwargs(opts)

    timer, fm = gettimer(ui, opts)

    def d():
        if clear:
            repo.invalidatevolatilesets()
        if contexts:
            for ctx in repo.set(expr):
                pass
        else:
            for r in repo.revs(expr):
                pass

    timer(d)
    fm.end()


@command(
    b'perf::volatilesets|perfvolatilesets',
    [
        (b'', b'clear-obsstore', False, b'drop obsstore between each call.'),
    ]
    + formatteropts,
)
def perfvolatilesets(ui, repo, *names, **opts):
    """benchmark the computation of various volatile set

    Volatile set computes element related to filtering and obsolescence."""
    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    repo = repo.unfiltered()

    def getobs(name):
        def d():
            repo.invalidatevolatilesets()
            if opts[b'clear_obsstore']:
                clearfilecache(repo, b'obsstore')
            obsolete.getrevs(repo, name)

        return d

    allobs = sorted(obsolete.cachefuncs)
    if names:
        allobs = [n for n in allobs if n in names]

    for name in allobs:
        timer(getobs(name), title=name)

    def getfiltered(name):
        def d():
            repo.invalidatevolatilesets()
            if opts[b'clear_obsstore']:
                clearfilecache(repo, b'obsstore')
            repoview.filterrevs(repo, name)

        return d

    allfilter = sorted(repoview.filtertable)
    if names:
        allfilter = [n for n in allfilter if n in names]

    for name in allfilter:
        timer(getfiltered(name), title=name)
    fm.end()


@command(
    b'perf::branchmap|perfbranchmap',
    [
        (b'f', b'full', False, b'Includes build time of subset'),
        (
            b'',
            b'clear-revbranch',
            False,
            b'purge the revbranch cache between computation',
        ),
    ]
    + formatteropts,
)
def perfbranchmap(ui, repo, *filternames, **opts):
    """benchmark the update of a branchmap

    This benchmarks the full repo.branchmap() call with read and write disabled
    """
    opts = _byteskwargs(opts)
    full = opts.get(b"full", False)
    clear_revbranch = opts.get(b"clear_revbranch", False)
    timer, fm = gettimer(ui, opts)

    def getbranchmap(filtername):
        """generate a benchmark function for the filtername"""
        if filtername is None:
            view = repo
        else:
            view = repo.filtered(filtername)
        if util.safehasattr(view._branchcaches, '_per_filter'):
            filtered = view._branchcaches._per_filter
        else:
            # older versions
            filtered = view._branchcaches

        def d():
            if clear_revbranch:
                repo.revbranchcache()._clear()
            if full:
                view._branchcaches.clear()
            else:
                filtered.pop(filtername, None)
            view.branchmap()

        return d

    # add filter in smaller subset to bigger subset
    possiblefilters = set(repoview.filtertable)
    if filternames:
        possiblefilters &= set(filternames)
    subsettable = getbranchmapsubsettable()
    allfilters = []
    while possiblefilters:
        for name in possiblefilters:
            subset = subsettable.get(name)
            if subset not in possiblefilters:
                break
        else:
            assert False, b'subset cycle %s!' % possiblefilters
        allfilters.append(name)
        possiblefilters.remove(name)

    # warm the cache
    if not full:
        for name in allfilters:
            repo.filtered(name).branchmap()
    if not filternames or b'unfiltered' in filternames:
        # add unfiltered
        allfilters.append(None)

    if util.safehasattr(branchmap.branchcache, 'fromfile'):
        branchcacheread = safeattrsetter(branchmap.branchcache, b'fromfile')
        branchcacheread.set(classmethod(lambda *args: None))
    else:
        # older versions
        branchcacheread = safeattrsetter(branchmap, b'read')
        branchcacheread.set(lambda *args: None)
    branchcachewrite = safeattrsetter(branchmap.branchcache, b'write')
    branchcachewrite.set(lambda *args: None)
    try:
        for name in allfilters:
            printname = name
            if name is None:
                printname = b'unfiltered'
            timer(getbranchmap(name), title=printname)
    finally:
        branchcacheread.restore()
        branchcachewrite.restore()
    fm.end()


@command(
    b'perf::branchmapupdate|perfbranchmapupdate',
    [
        (b'', b'base', [], b'subset of revision to start from'),
        (b'', b'target', [], b'subset of revision to end with'),
        (b'', b'clear-caches', False, b'clear cache between each runs'),
    ]
    + formatteropts,
)
def perfbranchmapupdate(ui, repo, base=(), target=(), **opts):
    """benchmark branchmap update from for <base> revs to <target> revs

    If `--clear-caches` is passed, the following items will be reset before
    each update:
        * the changelog instance and associated indexes
        * the rev-branch-cache instance

    Examples:

       # update for the one last revision
       $ hg perfbranchmapupdate --base 'not tip' --target 'tip'

       $ update for change coming with a new branch
       $ hg perfbranchmapupdate --base 'stable' --target 'default'
    """
    from mercurial import branchmap
    from mercurial import repoview

    opts = _byteskwargs(opts)
    timer, fm = gettimer(ui, opts)
    clearcaches = opts[b'clear_caches']
    unfi = repo.unfiltered()
    x = [None]  # used to pass data between closure

    # we use a `list` here to avoid possible side effect from smartset
    baserevs = list(scmutil.revrange(repo, base))
    targetrevs = list(scmutil.revrange(repo, target))
    if not baserevs:
        raise error.Abort(b'no revisions selected for --base')
    if not targetrevs:
        raise error.Abort(b'no revisions selected for --target')

    # make sure the target branchmap also contains the one in the base
    targetrevs = list(set(baserevs) | set(targetrevs))
    targetrevs.sort()

    cl = repo.changelog
    allbaserevs = list(cl.ancestors(baserevs, inclusive=True))
    allbaserevs.sort()
    alltargetrevs = frozenset(cl.ancestors(targetrevs, inclusive=True))

    newrevs = list(alltargetrevs.difference(allbaserevs))
    newrevs.sort()

    allrevs = frozenset(unfi.changelog.revs())
    basefilterrevs = frozenset(allrevs.difference(allbaserevs))
    targetfilterrevs = frozenset(allrevs.difference(alltargetrevs))

    def basefilter(repo, visibilityexceptions=None):
        return basefilterrevs

    def targetfilter(repo, visibilityexceptions=None):
        return targetfilterrevs

    msg = b'benchmark of branchmap with %d revisions with %d new ones\n'
    ui.status(msg % (len(allbaserevs), len(newrevs)))
    if targetfilterrevs:
        msg = b'(%d revisions still filtered)\n'
        ui.status(msg % len(targetfilterrevs))

    try:
        repoview.filtertable[b'__perf_branchmap_update_base'] = basefilter
        repoview.filtertable[b'__perf_branchmap_update_target'] = targetfilter

        baserepo = repo.filtered(b'__perf_branchmap_update_base')
        targetrepo = repo.filtered(b'__perf_branchmap_update_target')

        # try to find an existing branchmap to reuse
        subsettable = getbranchmapsubsettable()
        candidatefilter = subsettable.get(None)
        while candidatefilter is not None:
            candidatebm = repo.filtered(candidatefilter).branchmap()
            if candidatebm.validfor(baserepo):
                filtered = repoview.filterrevs(repo, candidatefilter)
                missing = [r for r in allbaserevs if r in filtered]
                base = candidatebm.copy()
                base.update(baserepo, missing)
                break
            candidatefilter = subsettable.get(candidatefilter)
        else:
            # no suitable subset where found
            base = branchmap.branchcache()
            base.update(baserepo, allbaserevs)

        def setup():
            x[0] = base.copy()
            if clearcaches:
                unfi._revbranchcache = None
                clearchangelog(repo)

        def bench():
            x[0].update(targetrepo, newrevs)

        timer(bench, setup=setup)
        fm.end()
    finally:
        repoview.filtertable.pop(b'__perf_branchmap_update_base', None)
        repoview.filtertable.pop(b'__perf_branchmap_update_target', None)


@command(
    b'perf::branchmapload|perfbranchmapload',
    [
        (b'f', b'filter', b'', b'Specify repoview filter'),
        (b'', b'list', False, b'List brachmap filter caches'),
        (b'', b'clear-revlogs', False, b'refresh changelog and manifest'),
    ]
    + formatteropts,
)
def perfbranchmapload(ui, repo, filter=b'', list=False, **opts):
    """benchmark reading the branchmap"""
    opts = _byteskwargs(opts)
    clearrevlogs = opts[b'clear_revlogs']

    if list:
        for name, kind, st in repo.cachevfs.readdir(stat=True):
            if name.startswith(b'branch2'):
                filtername = name.partition(b'-')[2] or b'unfiltered'
                ui.status(
                    b'%s - %s\n' % (filtername, util.bytecount(st.st_size))
                )
        return
    if not filter:
        filter = None
    subsettable = getbranchmapsubsettable()
    if filter is None:
        repo = repo.unfiltered()
    else:
        repo = repoview.repoview(repo, filter)

    repo.branchmap()  # make sure we have a relevant, up to date branchmap

    try:
        fromfile = branchmap.branchcache.fromfile
    except AttributeError:
        # older versions
        fromfile = branchmap.read

    currentfilter = filter
    # try once without timer, the filter may not be cached
    while fromfile(repo) is None:
        currentfilter = subsettable.get(currentfilter)
        if currentfilter is None:
            raise error.Abort(
                b'No branchmap cached for %s repo' % (filter or b'unfiltered')
            )
        repo = repo.filtered(currentfilter)
    timer, fm = gettimer(ui, opts)

    def setup():
        if clearrevlogs:
            clearchangelog(repo)

    def bench():
        fromfile(repo)

    timer(bench, setup=setup)
    fm.end()


@command(b'perf::loadmarkers|perfloadmarkers')
def perfloadmarkers(ui, repo):
    """benchmark the time to parse the on-disk markers for a repo

    Result is the number of markers in the repo."""
    timer, fm = gettimer(ui)
    svfs = getsvfs(repo)
    timer(lambda: len(obsolete.obsstore(repo, svfs)))
    fm.end()


@command(
    b'perf::lrucachedict|perflrucachedict',
    formatteropts
    + [
        (b'', b'costlimit', 0, b'maximum total cost of items in cache'),
        (b'', b'mincost', 0, b'smallest cost of items in cache'),
        (b'', b'maxcost', 100, b'maximum cost of items in cache'),
        (b'', b'size', 4, b'size of cache'),
        (b'', b'gets', 10000, b'number of key lookups'),
        (b'', b'sets', 10000, b'number of key sets'),
        (b'', b'mixed', 10000, b'number of mixed mode operations'),
        (
            b'',
            b'mixedgetfreq',
            50,
            b'frequency of get vs set ops in mixed mode',
        ),
    ],
    norepo=True,
)
def perflrucache(
    ui,
    mincost=0,
    maxcost=100,
    costlimit=0,
    size=4,
    gets=10000,
    sets=10000,
    mixed=10000,
    mixedgetfreq=50,
    **opts
):
    opts = _byteskwargs(opts)

    def doinit():
        for i in _xrange(10000):
            util.lrucachedict(size)

    costrange = list(range(mincost, maxcost + 1))

    values = []
    for i in _xrange(size):
        values.append(random.randint(0, _maxint))

    # Get mode fills the cache and tests raw lookup performance with no
    # eviction.
    getseq = []
    for i in _xrange(gets):
        getseq.append(random.choice(values))

    def dogets():
        d = util.lrucachedict(size)
        for v in values:
            d[v] = v
        for key in getseq:
            value = d[key]
            value  # silence pyflakes warning

    def dogetscost():
        d = util.lrucachedict(size, maxcost=costlimit)
        for i, v in enumerate(values):
            d.insert(v, v, cost=costs[i])
        for key in getseq:
            try:
                value = d[key]
                value  # silence pyflakes warning
            except KeyError:
                pass

    # Set mode tests insertion speed with cache eviction.
    setseq = []
    costs = []
    for i in _xrange(sets):
        setseq.append(random.randint(0, _maxint))
        costs.append(random.choice(costrange))

    def doinserts():
        d = util.lrucachedict(size)
        for v in setseq:
            d.insert(v, v)

    def doinsertscost():
        d = util.lrucachedict(size, maxcost=costlimit)
        for i, v in enumerate(setseq):
            d.insert(v, v, cost=costs[i])

    def dosets():
        d = util.lrucachedict(size)
        for v in setseq:
            d[v] = v

    # Mixed mode randomly performs gets and sets with eviction.
    mixedops = []
    for i in _xrange(mixed):
        r = random.randint(0, 100)
        if r < mixedgetfreq:
            op = 0
        else:
            op = 1

        mixedops.append(
            (op, random.randint(0, size * 2), random.choice(costrange))
        )

    def domixed():
        d = util.lrucachedict(size)

        for op, v, cost in mixedops:
            if op == 0:
                try:
                    d[v]
                except KeyError:
                    pass
            else:
                d[v] = v

    def domixedcost():
        d = util.lrucachedict(size, maxcost=costlimit)

        for op, v, cost in mixedops:
            if op == 0:
                try:
                    d[v]
                except KeyError:
                    pass
            else:
                d.insert(v, v, cost=cost)

    benches = [
        (doinit, b'init'),
    ]

    if costlimit:
        benches.extend(
            [
                (dogetscost, b'gets w/ cost limit'),
                (doinsertscost, b'inserts w/ cost limit'),
                (domixedcost, b'mixed w/ cost limit'),
            ]
        )
    else:
        benches.extend(
            [
                (dogets, b'gets'),
                (doinserts, b'inserts'),
                (dosets, b'sets'),
                (domixed, b'mixed'),
            ]
        )

    for fn, title in benches:
        timer, fm = gettimer(ui, opts)
        timer(fn, title=title)
        fm.end()


@command(
    b'perf::write|perfwrite',
    formatteropts
    + [
        (b'', b'write-method', b'write', b'ui write method'),
        (b'', b'nlines', 100, b'number of lines'),
        (b'', b'nitems', 100, b'number of items (per line)'),
        (b'', b'item', b'x', b'item that is written'),
        (b'', b'batch-line', None, b'pass whole line to write method at once'),
        (b'', b'flush-line', None, b'flush after each line'),
    ],
)
def perfwrite(ui, repo, **opts):
    """microbenchmark ui.write (and others)"""
    opts = _byteskwargs(opts)

    write = getattr(ui, _sysstr(opts[b'write_method']))
    nlines = int(opts[b'nlines'])
    nitems = int(opts[b'nitems'])
    item = opts[b'item']
    batch_line = opts.get(b'batch_line')
    flush_line = opts.get(b'flush_line')

    if batch_line:
        line = item * nitems + b'\n'

    def benchmark():
        for i in pycompat.xrange(nlines):
            if batch_line:
                write(line)
            else:
                for i in pycompat.xrange(nitems):
                    write(item)
                write(b'\n')
            if flush_line:
                ui.flush()
        ui.flush()

    timer, fm = gettimer(ui, opts)
    timer(benchmark)
    fm.end()


def uisetup(ui):
    if util.safehasattr(cmdutil, b'openrevlog') and not util.safehasattr(
        commands, b'debugrevlogopts'
    ):
        # for "historical portability":
        # In this case, Mercurial should be 1.9 (or a79fea6b3e77) -
        # 3.7 (or 5606f7d0d063). Therefore, '--dir' option for
        # openrevlog() should cause failure, because it has been
        # available since 3.5 (or 49c583ca48c4).
        def openrevlog(orig, repo, cmd, file_, opts):
            if opts.get(b'dir') and not util.safehasattr(repo, b'dirlog'):
                raise error.Abort(
                    b"This version doesn't support --dir option",
                    hint=b"use 3.5 or later",
                )
            return orig(repo, cmd, file_, opts)

        extensions.wrapfunction(cmdutil, b'openrevlog', openrevlog)


@command(
    b'perf::progress|perfprogress',
    formatteropts
    + [
        (b'', b'topic', b'topic', b'topic for progress messages'),
        (b'c', b'total', 1000000, b'total value we are progressing to'),
    ],
    norepo=True,
)
def perfprogress(ui, topic=None, total=None, **opts):
    """printing of progress bars"""
    opts = _byteskwargs(opts)

    timer, fm = gettimer(ui, opts)

    def doprogress():
        with ui.makeprogress(topic, total=total) as progress:
            for i in _xrange(total):
                progress.increment()

    timer(doprogress)
    fm.end()
