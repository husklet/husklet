# dirstate.py - working directory tracking for mercurial
#
# Copyright 2005-2007 Olivia Mackall <olivia@selenic.com>
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.


import collections
import contextlib
import os
import stat
import uuid

from .i18n import _
from .pycompat import delattr

from hgdemandimport import tracing

from . import (
    dirstatemap,
    encoding,
    error,
    match as matchmod,
    node,
    pathutil,
    policy,
    pycompat,
    scmutil,
    util,
)

from .dirstateutils import (
    docket as docketmod,
    timestamp,
)

from .interfaces import (
    dirstate as intdirstate,
    util as interfaceutil,
)

parsers = policy.importmod('parsers')
rustmod = policy.importrust('dirstate')

HAS_FAST_DIRSTATE_V2 = rustmod is not None

propertycache = util.propertycache
filecache = scmutil.filecache
_rangemask = dirstatemap.rangemask

DirstateItem = dirstatemap.DirstateItem


class repocache(filecache):
    """filecache for files in .hg/"""

    def join(self, obj, fname):
        return obj._opener.join(fname)


class rootcache(filecache):
    """filecache for files in the repository root"""

    def join(self, obj, fname):
        return obj._join(fname)


def requires_parents_change(func):
    def wrap(self, *args, **kwargs):
        if not self.pendingparentchange():
            msg = 'calling `%s` outside of a parentchange context'
            msg %= func.__name__
            raise error.ProgrammingError(msg)
        return func(self, *args, **kwargs)

    return wrap


def requires_no_parents_change(func):
    def wrap(self, *args, **kwargs):
        if self.pendingparentchange():
            msg = 'calling `%s` inside of a parentchange context'
            msg %= func.__name__
            raise error.ProgrammingError(msg)
        return func(self, *args, **kwargs)

    return wrap


@interfaceutil.implementer(intdirstate.idirstate)
class dirstate:
    def __init__(
        self,
        opener,
        ui,
        root,
        validate,
        sparsematchfn,
        nodeconstants,
        use_dirstate_v2,
        use_tracked_hint=False,
    ):
        """Create a new dirstate object.

        opener is an open()-like callable that can be used to open the
        dirstate file; root is the root of the directory tracked by
        the dirstate.
        """
        self._use_dirstate_v2 = use_dirstate_v2
        self._use_tracked_hint = use_tracked_hint
        self._nodeconstants = nodeconstants
        self._opener = opener
        self._validate = validate
        self._root = root
        # Either build a sparse-matcher or None if sparse is disabled
        self._sparsematchfn = sparsematchfn
        # ntpath.join(root, '') of Python 2.7.9 does not add sep if root is
        # UNC path pointing to root share (issue4557)
        self._rootdir = pathutil.normasprefix(root)
        # True is any internal state may be different
        self._dirty = False
        # True if the set of tracked file may be different
        self._dirty_tracked_set = False
        self._ui = ui
        self._filecache = {}
        self._parentwriters = 0
        self._filename = b'dirstate'
        self._filename_th = b'dirstate-tracked-hint'
        self._pendingfilename = b'%s.pending' % self._filename
        self._plchangecallbacks = {}
        self._origpl = None
        self._mapcls = dirstatemap.dirstatemap
        # Access and cache cwd early, so we don't access it for the first time
        # after a working-copy update caused it to not exist (accessing it then
        # raises an exception).
        self._cwd

    def prefetch_parents(self):
        """make sure the parents are loaded

        Used to avoid a race condition.
        """
        self._pl

    @contextlib.contextmanager
    def parentchange(self):
        """Context manager for handling dirstate parents.

        If an exception occurs in the scope of the context manager,
        the incoherent dirstate won't be written when wlock is
        released.
        """
        self._parentwriters += 1
        yield
        # Typically we want the "undo" step of a context manager in a
        # finally block so it happens even when an exception
        # occurs. In this case, however, we only want to decrement
        # parentwriters if the code in the with statement exits
        # normally, so we don't have a try/finally here on purpose.
        self._parentwriters -= 1

    def pendingparentchange(self):
        """Returns true if the dirstate is in the middle of a set of changes
        that modify the dirstate parent.
        """
        return self._parentwriters > 0

    @propertycache
    def _map(self):
        """Return the dirstate contents (see documentation for dirstatemap)."""
        self._map = self._mapcls(
            self._ui,
            self._opener,
            self._root,
            self._nodeconstants,
            self._use_dirstate_v2,
        )
        return self._map

    @property
    def _sparsematcher(self):
        """The matcher for the sparse checkout.

        The working directory may not include every file from a manifest. The
        matcher obtained by this property will match a path if it is to be
        included in the working directory.

        When sparse if disabled, return None.
        """
        if self._sparsematchfn is None:
            return None
        # TODO there is potential to cache this property. For now, the matcher
        # is resolved on every access. (But the called function does use a
        # cache to keep the lookup fast.)
        return self._sparsematchfn()

    @repocache(b'branch')
    def _branch(self):
        try:
            return self._opener.read(b"branch").strip() or b"default"
        except FileNotFoundError:
            return b"default"

    @property
    def _pl(self):
        return self._map.parents()

    def hasdir(self, d):
        return self._map.hastrackeddir(d)

    @rootcache(b'.hgignore')
    def _ignore(self):
        files = self._ignorefiles()
        if not files:
            return matchmod.never()

        pats = [b'include:%s' % f for f in files]
        return matchmod.match(self._root, b'', [], pats, warn=self._ui.warn)

    @propertycache
    def _slash(self):
        return self._ui.configbool(b'ui', b'slash') and pycompat.ossep != b'/'

    @propertycache
    def _checklink(self):
        return util.checklink(self._root)

    @propertycache
    def _checkexec(self):
        return bool(util.checkexec(self._root))

    @propertycache
    def _checkcase(self):
        return not util.fscasesensitive(self._join(b'.hg'))

    def _join(self, f):
        # much faster than os.path.join()
        # it's safe because f is always a relative path
        return self._rootdir + f

    def flagfunc(self, buildfallback):
        """build a callable that returns flags associated with a filename

        The information is extracted from three possible layers:
        1. the file system if it supports the information
        2. the "fallback" information stored in the dirstate if any
        3. a more expensive mechanism inferring the flags from the parents.
        """

        # small hack to cache the result of buildfallback()
        fallback_func = []

        def get_flags(x):
            entry = None
            fallback_value = None
            try:
                st = os.lstat(self._join(x))
            except OSError:
                return b''

            if self._checklink:
                if util.statislink(st):
                    return b'l'
            else:
                entry = self.get_entry(x)
                if entry.has_fallback_symlink:
                    if entry.fallback_symlink:
                        return b'l'
                else:
                    if not fallback_func:
                        fallback_func.append(buildfallback())
                    fallback_value = fallback_func[0](x)
                    if b'l' in fallback_value:
                        return b'l'

            if self._checkexec:
                if util.statisexec(st):
                    return b'x'
            else:
                if entry is None:
                    entry = self.get_entry(x)
                if entry.has_fallback_exec:
                    if entry.fallback_exec:
                        return b'x'
                else:
                    if fallback_value is None:
                        if not fallback_func:
                            fallback_func.append(buildfallback())
                        fallback_value = fallback_func[0](x)
                    if b'x' in fallback_value:
                        return b'x'
            return b''

        return get_flags

    @propertycache
    def _cwd(self):
        # internal config: ui.forcecwd
        forcecwd = self._ui.config(b'ui', b'forcecwd')
        if forcecwd:
            return forcecwd
        return encoding.getcwd()

    def getcwd(self):
        """Return the path from which a canonical path is calculated.

        This path should be used to resolve file patterns or to convert
        canonical paths back to file paths for display. It shouldn't be
        used to get real file paths. Use vfs functions instead.
        """
        cwd = self._cwd
        if cwd == self._root:
            return b''
        # self._root ends with a path separator if self._root is '/' or 'C:\'
        rootsep = self._root
        if not util.endswithsep(rootsep):
            rootsep += pycompat.ossep
        if cwd.startswith(rootsep):
            return cwd[len(rootsep) :]
        else:
            # we're outside the repo. return an absolute path.
            return cwd

    def pathto(self, f, cwd=None):
        if cwd is None:
            cwd = self.getcwd()
        path = util.pathto(self._root, cwd, f)
        if self._slash:
            return util.pconvert(path)
        return path

    def get_entry(self, path):
        """return a DirstateItem for the associated path"""
        entry = self._map.get(path)
        if entry is None:
            return DirstateItem()
        return entry

    def __contains__(self, key):
        return key in self._map

    def __iter__(self):
        return iter(sorted(self._map))

    def items(self):
        return self._map.items()

    iteritems = items

    def parents(self):
        return [self._validate(p) for p in self._pl]

    def p1(self):
        return self._validate(self._pl[0])

    def p2(self):
        return self._validate(self._pl[1])

    @property
    def in_merge(self):
        """True if a merge is in progress"""
        return self._pl[1] != self._nodeconstants.nullid

    def branch(self):
        return encoding.tolocal(self._branch)

    def setparents(self, p1, p2=None):
        """Set dirstate parents to p1 and p2.

        When moving from two parents to one, "merged" entries a
        adjusted to normal and previous copy records discarded and
        returned by the call.

        See localrepo.setparents()
        """
        if p2 is None:
            p2 = self._nodeconstants.nullid
        if self._parentwriters == 0:
            raise ValueError(
                b"cannot set dirstate parent outside of "
                b"dirstate.parentchange context manager"
            )

        self._dirty = True
        oldp2 = self._pl[1]
        if self._origpl is None:
            self._origpl = self._pl
        nullid = self._nodeconstants.nullid
        # True if we need to fold p2 related state back to a linear case
        fold_p2 = oldp2 != nullid and p2 == nullid
        return self._map.setparents(p1, p2, fold_p2=fold_p2)

    def setbranch(self, branch):
        self.__class__._branch.set(self, encoding.fromlocal(branch))
        f = self._opener(b'branch', b'w', atomictemp=True, checkambig=True)
        try:
            f.write(self._branch + b'\n')
            f.close()

            # make sure filecache has the correct stat info for _branch after
            # replacing the underlying file
            ce = self._filecache[b'_branch']
            if ce:
                ce.refresh()
        except:  # re-raises
            f.discard()
            raise

    def invalidate(self):
        """Causes the next access to reread the dirstate.

        This is different from localrepo.invalidatedirstate() because it always
        rereads the dirstate. Use localrepo.invalidatedirstate() if you want to
        check whether the dirstate has changed before rereading it."""

        for a in ("_map", "_branch", "_ignore"):
            if a in self.__dict__:
                delattr(self, a)
        self._dirty = False
        self._dirty_tracked_set = False
        self._parentwriters = 0
        self._origpl = None

    def copy(self, source, dest):
        """Mark dest as a copy of source. Unmark dest if source is None."""
        if source == dest:
            return
        self._dirty = True
        if source is not None:
            self._check_sparse(source)
            self._map.copymap[dest] = source
        else:
            self._map.copymap.pop(dest, None)

    def copied(self, file):
        return self._map.copymap.get(file, None)

    def copies(self):
        return self._map.copymap

    @requires_no_parents_change
    def set_tracked(self, filename, reset_copy=False):
        """a "public" method for generic code to mark a file as tracked

        This function is to be called outside of "update/merge" case. For
        example by a command like `hg add X`.

        if reset_copy is set, any existing copy information will be dropped.

        return True the file was previously untracked, False otherwise.
        """
        self._dirty = True
        entry = self._map.get(filename)
        if entry is None or not entry.tracked:
            self._check_new_tracked_filename(filename)
        pre_tracked = self._map.set_tracked(filename)
        if reset_copy:
            self._map.copymap.pop(filename, None)
        if pre_tracked:
            self._dirty_tracked_set = True
        return pre_tracked

    @requires_no_parents_change
    def set_untracked(self, filename):
        """a "public" method for generic code to mark a file as untracked

        This function is to be called outside of "update/merge" case. For
        example by a command like `hg remove X`.

        return True the file was previously tracked, False otherwise.
        """
        ret = self._map.set_untracked(filename)
        if ret:
            self._dirty = True
            self._dirty_tracked_set = True
        return ret

    @requires_no_parents_change
    def set_clean(self, filename, parentfiledata):
        """record that the current state of the file on disk is known to be clean"""
        self._dirty = True
        if not self._map[filename].tracked:
            self._check_new_tracked_filename(filename)
        (mode, size, mtime) = parentfiledata
        self._map.set_clean(filename, mode, size, mtime)

    @requires_no_parents_change
    def set_possibly_dirty(self, filename):
        """record that the current state of the file on disk is unknown"""
        self._dirty = True
        self._map.set_possibly_dirty(filename)

    @requires_parents_change
    def update_file_p1(
        self,
        filename,
        p1_tracked,
    ):
        """Set a file as tracked in the parent (or not)

        This is to be called when adjust the dirstate to a new parent after an history
        rewriting operation.

        It should not be called during a merge (p2 != nullid) and only within
        a `with dirstate.parentchange():` context.
        """
        if self.in_merge:
            msg = b'update_file_reference should not be called when merging'
            raise error.ProgrammingError(msg)
        entry = self._map.get(filename)
        if entry is None:
            wc_tracked = False
        else:
            wc_tracked = entry.tracked
        if not (p1_tracked or wc_tracked):
            # the file is no longer relevant to anyone
            if self._map.get(filename) is not None:
                self._map.reset_state(filename)
                self._dirty = True
        elif (not p1_tracked) and wc_tracked:
            if entry is not None and entry.added:
                return  # avoid dropping copy information (maybe?)

        self._map.reset_state(
            filename,
            wc_tracked,
            p1_tracked,
            # the underlying reference might have changed, we will have to
            # check it.
            has_meaningful_mtime=False,
        )

    @requires_parents_change
    def update_file(
        self,
        filename,
        wc_tracked,
        p1_tracked,
        p2_info=False,
        possibly_dirty=False,
        parentfiledata=None,
    ):
        """update the information about a file in the dirstate

        This is to be called when the direstates parent changes to keep track
        of what is the file situation in regards to the working copy and its parent.

        This function must be called within a `dirstate.parentchange` context.

        note: the API is at an early stage and we might need to adjust it
        depending of what information ends up being relevant and useful to
        other processing.
        """

        # note: I do not think we need to double check name clash here since we
        # are in a update/merge case that should already have taken care of
        # this. The test agrees

        self._dirty = True
        old_entry = self._map.get(filename)
        if old_entry is None:
            prev_tracked = False
        else:
            prev_tracked = old_entry.tracked
        if prev_tracked != wc_tracked:
            self._dirty_tracked_set = True

        self._map.reset_state(
            filename,
            wc_tracked,
            p1_tracked,
            p2_info=p2_info,
            has_meaningful_mtime=not possibly_dirty,
            parentfiledata=parentfiledata,
        )

    def _check_new_tracked_filename(self, filename):
        scmutil.checkfilename(filename)
        if self._map.hastrackeddir(filename):
            msg = _(b'directory %r already in dirstate')
            msg %= pycompat.bytestr(filename)
            raise error.Abort(msg)
        # shadows
        for d in pathutil.finddirs(filename):
            if self._map.hastrackeddir(d):
                break
            entry = self._map.get(d)
            if entry is not None and not entry.removed:
                msg = _(b'file %r in dirstate clashes with %r')
                msg %= (pycompat.bytestr(d), pycompat.bytestr(filename))
                raise error.Abort(msg)
        self._check_sparse(filename)

    def _check_sparse(self, filename):
        """Check that a filename is inside the sparse profile"""
        sparsematch = self._sparsematcher
        if sparsematch is not None and not sparsematch.always():
            if not sparsematch(filename):
                msg = _(b"cannot add '%s' - it is outside the sparse checkout")
                hint = _(
                    b'include file with `hg debugsparse --include <pattern>` or use '
                    b'`hg add -s <file>` to include file directory while adding'
                )
                raise error.Abort(msg % filename, hint=hint)

    def _discoverpath(self, path, normed, ignoremissing, exists, storemap):
        if exists is None:
            exists = os.path.lexists(os.path.join(self._root, path))
        if not exists:
            # Maybe a path component exists
            if not ignoremissing and b'/' in path:
                d, f = path.rsplit(b'/', 1)
                d = self._normalize(d, False, ignoremissing, None)
                folded = d + b"/" + f
            else:
                # No path components, preserve original case
                folded = path
        else:
            # recursively normalize leading directory components
            # against dirstate
            if b'/' in normed:
                d, f = normed.rsplit(b'/', 1)
                d = self._normalize(d, False, ignoremissing, True)
                r = self._root + b"/" + d
                folded = d + b"/" + util.fspath(f, r)
            else:
                folded = util.fspath(normed, self._root)
            storemap[normed] = folded

        return folded

    def _normalizefile(self, path, isknown, ignoremissing=False, exists=None):
        normed = util.normcase(path)
        folded = self._map.filefoldmap.get(normed, None)
        if folded is None:
            if isknown:
                folded = path
            else:
                folded = self._discoverpath(
                    path, normed, ignoremissing, exists, self._map.filefoldmap
                )
        return folded

    def _normalize(self, path, isknown, ignoremissing=False, exists=None):
        normed = util.normcase(path)
        folded = self._map.filefoldmap.get(normed, None)
        if folded is None:
            folded = self._map.dirfoldmap.get(normed, None)
        if folded is None:
            if isknown:
                folded = path
            else:
                # store discovered result in dirfoldmap so that future
                # normalizefile calls don't start matching directories
                folded = self._discoverpath(
                    path, normed, ignoremissing, exists, self._map.dirfoldmap
                )
        return folded

    def normalize(self, path, isknown=False, ignoremissing=False):
        """
        normalize the case of a pathname when on a casefolding filesystem

        isknown specifies whether the filename came from walking the
        disk, to avoid extra filesystem access.

        If ignoremissing is True, missing path are returned
        unchanged. Otherwise, we try harder to normalize possibly
        existing path components.

        The normalized case is determined based on the following precedence:

        - version of name already stored in the dirstate
        - version of name stored on disk
        - version provided via command arguments
        """

        if self._checkcase:
            return self._normalize(path, isknown, ignoremissing)
        return path

    def clear(self):
        self._map.clear()
        self._dirty = True

    def rebuild(self, parent, allfiles, changedfiles=None):

        matcher = self._sparsematcher
        if matcher is not None and not matcher.always():
            # should not add non-matching files
            allfiles = [f for f in allfiles if matcher(f)]
            if changedfiles:
                changedfiles = [f for f in changedfiles if matcher(f)]

            if changedfiles is not None:
                # these files will be deleted from the dirstate when they are
                # not found to be in allfiles
                dirstatefilestoremove = {f for f in self if not matcher(f)}
                changedfiles = dirstatefilestoremove.union(changedfiles)

        if changedfiles is None:
            # Rebuild entire dirstate
            to_lookup = allfiles
            to_drop = []
            self.clear()
        elif len(changedfiles) < 10:
            # Avoid turning allfiles into a set, which can be expensive if it's
            # large.
            to_lookup = []
            to_drop = []
            for f in changedfiles:
                if f in allfiles:
                    to_lookup.append(f)
                else:
                    to_drop.append(f)
        else:
            changedfilesset = set(changedfiles)
            to_lookup = changedfilesset & set(allfiles)
            to_drop = changedfilesset - to_lookup

        if self._origpl is None:
            self._origpl = self._pl
        self._map.setparents(parent, self._nodeconstants.nullid)

        for f in to_lookup:

            if self.in_merge:
                self.set_tracked(f)
            else:
                self._map.reset_state(
                    f,
                    wc_tracked=True,
                    p1_tracked=True,
                )
        for f in to_drop:
            self._map.reset_state(f)

        self._dirty = True

    def identity(self):
        """Return identity of dirstate itself to detect changing in storage

        If identity of previous dirstate is equal to this, writing
        changes based on the former dirstate out can keep consistency.
        """
        return self._map.identity

    def write(self, tr):
        if not self._dirty:
            return

        write_key = self._use_tracked_hint and self._dirty_tracked_set
        if tr:
            # delay writing in-memory changes out
            tr.addfilegenerator(
                b'dirstate-1-main',
                (self._filename,),
                lambda f: self._writedirstate(tr, f),
                location=b'plain',
                post_finalize=True,
            )
            if write_key:
                tr.addfilegenerator(
                    b'dirstate-2-key-post',
                    (self._filename_th,),
                    lambda f: self._write_tracked_hint(tr, f),
                    location=b'plain',
                    post_finalize=True,
                )
            return

        file = lambda f: self._opener(f, b"w", atomictemp=True, checkambig=True)
        with file(self._filename) as f:
            self._writedirstate(tr, f)
        if write_key:
            # we update the key-file after writing to make sure reader have a
            # key that match the newly written content
            with file(self._filename_th) as f:
                self._write_tracked_hint(tr, f)

    def delete_tracked_hint(self):
        """remove the tracked_hint file

        To be used by format downgrades operation"""
        self._opener.unlink(self._filename_th)
        self._use_tracked_hint = False

    def addparentchangecallback(self, category, callback):
        """add a callback to be called when the wd parents are changed

        Callback will be called with the following arguments:
            dirstate, (oldp1, oldp2), (newp1, newp2)

        Category is a unique identifier to allow overwriting an old callback
        with a newer callback.
        """
        self._plchangecallbacks[category] = callback

    def _writedirstate(self, tr, st):
        # notify callbacks about parents change
        if self._origpl is not None and self._origpl != self._pl:
            for c, callback in sorted(self._plchangecallbacks.items()):
                callback(self, self._origpl, self._pl)
            self._origpl = None
        self._map.write(tr, st)
        self._dirty = False
        self._dirty_tracked_set = False

    def _write_tracked_hint(self, tr, f):
        key = node.hex(uuid.uuid4().bytes)
        f.write(b"1\n%s\n" % key)  # 1 is the format version

    def _dirignore(self, f):
        if self._ignore(f):
            return True
        for p in pathutil.finddirs(f):
            if self._ignore(p):
                return True
        return False

    def _ignorefiles(self):
        files = []
        if os.path.exists(self._join(b'.hgignore')):
            files.append(self._join(b'.hgignore'))
        for name, path in self._ui.configitems(b"ui"):
            if name == b'ignore' or name.startswith(b'ignore.'):
                # we need to use os.path.join here rather than self._join
                # because path is arbitrary and user-specified
                files.append(os.path.join(self._rootdir, util.expandpath(path)))
        return files

    def _ignorefileandline(self, f):
        files = collections.deque(self._ignorefiles())
        visited = set()
        while files:
            i = files.popleft()
            patterns = matchmod.readpatternfile(
                i, self._ui.warn, sourceinfo=True
            )
            for pattern, lineno, line in patterns:
                kind, p = matchmod._patsplit(pattern, b'glob')
                if kind == b"subinclude":
                    if p not in visited:
                        files.append(p)
                    continue
                m = matchmod.match(
                    self._root, b'', [], [pattern], warn=self._ui.warn
                )
                if m(f):
                    return (i, lineno, line)
            visited.add(i)
        return (None, -1, b"")

    def _walkexplicit(self, match, subrepos):
        """Get stat data about the files explicitly specified by match.

        Return a triple (results, dirsfound, dirsnotfound).
        - results is a mapping from filename to stat result. It also contains
          listings mapping subrepos and .hg to None.
        - dirsfound is a list of files found to be directories.
        - dirsnotfound is a list of files that the dirstate thinks are
          directories and that were not found."""

        def badtype(mode):
            kind = _(b'unknown')
            if stat.S_ISCHR(mode):
                kind = _(b'character device')
            elif stat.S_ISBLK(mode):
                kind = _(b'block device')
            elif stat.S_ISFIFO(mode):
                kind = _(b'fifo')
            elif stat.S_ISSOCK(mode):
                kind = _(b'socket')
            elif stat.S_ISDIR(mode):
                kind = _(b'directory')
            return _(b'unsupported file type (type is %s)') % kind

        badfn = match.bad
        dmap = self._map
        lstat = os.lstat
        getkind = stat.S_IFMT
        dirkind = stat.S_IFDIR
        regkind = stat.S_IFREG
        lnkkind = stat.S_IFLNK
        join = self._join
        dirsfound = []
        foundadd = dirsfound.append
        dirsnotfound = []
        notfoundadd = dirsnotfound.append

        if not match.isexact() and self._checkcase:
            normalize = self._normalize
        else:
            normalize = None

        files = sorted(match.files())
        subrepos.sort()
        i, j = 0, 0
        while i < len(files) and j < len(subrepos):
            subpath = subrepos[j] + b"/"
            if files[i] < subpath:
                i += 1
                continue
            while i < len(files) and files[i].startswith(subpath):
                del files[i]
            j += 1

        if not files or b'' in files:
            files = [b'']
            # constructing the foldmap is expensive, so don't do it for the
            # common case where files is ['']
            normalize = None
        results = dict.fromkeys(subrepos)
        results[b'.hg'] = None

        for ff in files:
            if normalize:
                nf = normalize(ff, False, True)
            else:
                nf = ff
            if nf in results:
                continue

            try:
                st = lstat(join(nf))
                kind = getkind(st.st_mode)
                if kind == dirkind:
                    if nf in dmap:
                        # file replaced by dir on disk but still in dirstate
                        results[nf] = None
                    foundadd((nf, ff))
                elif kind == regkind or kind == lnkkind:
                    results[nf] = st
                else:
                    badfn(ff, badtype(kind))
                    if nf in dmap:
                        results[nf] = None
            except OSError as inst:  # nf not found on disk - it is dirstate only
                if nf in dmap:  # does it exactly match a missing file?
                    results[nf] = None
                else:  # does it match a missing directory?
                    if self._map.hasdir(nf):
                        notfoundadd(nf)
                    else:
                        badfn(ff, encoding.strtolocal(inst.strerror))

        # match.files() may contain explicitly-specified paths that shouldn't
        # be taken; drop them from the list of files found. dirsfound/notfound
        # aren't filtered here because they will be tested later.
        if match.anypats():
            for f in list(results):
                if f == b'.hg' or f in subrepos:
                    # keep sentinel to disable further out-of-repo walks
                    continue
                if not match(f):
                    del results[f]

        # Case insensitive filesystems cannot rely on lstat() failing to detect
        # a case-only rename.  Prune the stat object for any file that does not
        # match the case in the filesystem, if there are multiple files that
        # normalize to the same path.
        if match.isexact() and self._checkcase:
            normed = {}

            for f, st in results.items():
                if st is None:
                    continue

                nc = util.normcase(f)
                paths = normed.get(nc)

                if paths is None:
                    paths = set()
                    normed[nc] = paths

                paths.add(f)

            for norm, paths in normed.items():
                if len(paths) > 1:
                    for path in paths:
                        folded = self._discoverpath(
                            path, norm, True, None, self._map.dirfoldmap
                        )
                        if path != folded:
                            results[path] = None

        return results, dirsfound, dirsnotfound

    def walk(self, match, subrepos, unknown, ignored, full=True):
        """
        Walk recursively through the directory tree, finding all files
        matched by match.

        If full is False, maybe skip some known-clean files.

        Return a dict mapping filename to stat-like object (either
        mercurial.osutil.stat instance or return value of os.stat()).

        """
        # full is a flag that extensions that hook into walk can use -- this
        # implementation doesn't use it at all. This satisfies the contract
        # because we only guarantee a "maybe".

        if ignored:
            ignore = util.never
            dirignore = util.never
        elif unknown:
            ignore = self._ignore
            dirignore = self._dirignore
        else:
            # if not unknown and not ignored, drop dir recursion and step 2
            ignore = util.always
            dirignore = util.always

        if self._sparsematchfn is not None:
            em = matchmod.exact(match.files())
            sm = matchmod.unionmatcher([self._sparsematcher, em])
            match = matchmod.intersectmatchers(match, sm)

        matchfn = match.matchfn
        matchalways = match.always()
        matchtdir = match.traversedir
        dmap = self._map
        listdir = util.listdir
        lstat = os.lstat
        dirkind = stat.S_IFDIR
        regkind = stat.S_IFREG
        lnkkind = stat.S_IFLNK
        join = self._join

        exact = skipstep3 = False
        if match.isexact():  # match.exact
            exact = True
            dirignore = util.always  # skip step 2
        elif match.prefix():  # match.match, no patterns
            skipstep3 = True

        if not exact and self._checkcase:
            normalize = self._normalize
            normalizefile = self._normalizefile
            skipstep3 = False
        else:
            normalize = self._normalize
            normalizefile = None

        # step 1: find all explicit files
        results, work, dirsnotfound = self._walkexplicit(match, subrepos)
        if matchtdir:
            for d in work:
                matchtdir(d[0])
            for d in dirsnotfound:
                matchtdir(d)

        skipstep3 = skipstep3 and not (work or dirsnotfound)
        work = [d for d in work if not dirignore(d[0])]

        # step 2: visit subdirectories
        def traverse(work, alreadynormed):
            wadd = work.append
            while work:
                tracing.counter('dirstate.walk work', len(work))
                nd = work.pop()
                visitentries = match.visitchildrenset(nd)
                if not visitentries:
                    continue
                if visitentries == b'this' or visitentries == b'all':
                    visitentries = None
                skip = None
                if nd != b'':
                    skip = b'.hg'
                try:
                    with tracing.log('dirstate.walk.traverse listdir %s', nd):
                        entries = listdir(join(nd), stat=True, skip=skip)
                except (PermissionError, FileNotFoundError) as inst:
                    match.bad(
                        self.pathto(nd), encoding.strtolocal(inst.strerror)
                    )
                    continue
                for f, kind, st in entries:
                    # Some matchers may return files in the visitentries set,
                    # instead of 'this', if the matcher explicitly mentions them
                    # and is not an exactmatcher. This is acceptable; we do not
                    # make any hard assumptions about file-or-directory below
                    # based on the presence of `f` in visitentries. If
                    # visitchildrenset returned a set, we can always skip the
                    # entries *not* in the set it provided regardless of whether
                    # they're actually a file or a directory.
                    if visitentries and f not in visitentries:
                        continue
                    if normalizefile:
                        # even though f might be a directory, we're only
                        # interested in comparing it to files currently in the
                        # dmap -- therefore normalizefile is enough
                        nf = normalizefile(
                            nd and (nd + b"/" + f) or f, True, True
                        )
                    else:
                        nf = nd and (nd + b"/" + f) or f
                    if nf not in results:
                        if kind == dirkind:
                            if not ignore(nf):
                                if matchtdir:
                                    matchtdir(nf)
                                wadd(nf)
                            if nf in dmap and (matchalways or matchfn(nf)):
                                results[nf] = None
                        elif kind == regkind or kind == lnkkind:
                            if nf in dmap:
                                if matchalways or matchfn(nf):
                                    results[nf] = st
                            elif (matchalways or matchfn(nf)) and not ignore(
                                nf
                            ):
                                # unknown file -- normalize if necessary
                                if not alreadynormed:
                                    nf = normalize(nf, False, True)
                                results[nf] = st
                        elif nf in dmap and (matchalways or matchfn(nf)):
                            results[nf] = None

        for nd, d in work:
            # alreadynormed means that processwork doesn't have to do any
            # expensive directory normalization
            alreadynormed = not normalize or nd == d
            traverse([d], alreadynormed)

        for s in subrepos:
            del results[s]
        del results[b'.hg']

        # step 3: visit remaining files from dmap
        if not skipstep3 and not exact:
            # If a dmap file is not in results yet, it was either
            # a) not matching matchfn b) ignored, c) missing, or d) under a
            # symlink directory.
            if not results and matchalways:
                visit = [f for f in dmap]
            else:
                visit = [f for f in dmap if f not in results and matchfn(f)]
            visit.sort()

            if unknown:
                # unknown == True means we walked all dirs under the roots
                # that wasn't ignored, and everything that matched was stat'ed
                # and is already in results.
                # The rest must thus be ignored or under a symlink.
                audit_path = pathutil.pathauditor(self._root, cached=True)

                for nf in iter(visit):
                    # If a stat for the same file was already added with a
                    # different case, don't add one for this, since that would
                    # make it appear as if the file exists under both names
                    # on disk.
                    if (
                        normalizefile
                        and normalizefile(nf, True, True) in results
                    ):
                        results[nf] = None
                    # Report ignored items in the dmap as long as they are not
                    # under a symlink directory.
                    elif audit_path.check(nf):
                        try:
                            results[nf] = lstat(join(nf))
                            # file was just ignored, no links, and exists
                        except OSError:
                            # file doesn't exist
                            results[nf] = None
                    else:
                        # It's either missing or under a symlink directory
                        # which we in this case report as missing
                        results[nf] = None
            else:
                # We may not have walked the full directory tree above,
                # so stat and check everything we missed.
                iv = iter(visit)
                for st in util.statfiles([join(i) for i in visit]):
                    results[next(iv)] = st
        return results

    def _rust_status(self, matcher, list_clean, list_ignored, list_unknown):
        if self._sparsematchfn is not None:
            em = matchmod.exact(matcher.files())
            sm = matchmod.unionmatcher([self._sparsematcher, em])
            matcher = matchmod.intersectmatchers(matcher, sm)
        # Force Rayon (Rust parallelism library) to respect the number of
        # workers. This is a temporary workaround until Rust code knows
        # how to read the config file.
        numcpus = self._ui.configint(b"worker", b"numcpus")
        if numcpus is not None:
            encoding.environ.setdefault(b'RAYON_NUM_THREADS', b'%d' % numcpus)

        workers_enabled = self._ui.configbool(b"worker", b"enabled", True)
        if not workers_enabled:
            encoding.environ[b"RAYON_NUM_THREADS"] = b"1"

        (
            lookup,
            modified,
            added,
            removed,
            deleted,
            clean,
            ignored,
            unknown,
            warnings,
            bad,
            traversed,
            dirty,
        ) = rustmod.status(
            self._map._map,
            matcher,
            self._rootdir,
            self._ignorefiles(),
            self._checkexec,
            bool(list_clean),
            bool(list_ignored),
            bool(list_unknown),
            bool(matcher.traversedir),
        )

        self._dirty |= dirty

        if matcher.traversedir:
            for dir in traversed:
                matcher.traversedir(dir)

        if self._ui.warn:
            for item in warnings:
                if isinstance(item, tuple):
                    file_path, syntax = item
                    msg = _(b"%s: ignoring invalid syntax '%s'\n") % (
                        file_path,
                        syntax,
                    )
                    self._ui.warn(msg)
                else:
                    msg = _(b"skipping unreadable pattern file '%s': %s\n")
                    self._ui.warn(
                        msg
                        % (
                            pathutil.canonpath(
                                self._rootdir, self._rootdir, item
                            ),
                            b"No such file or directory",
                        )
                    )

        for (fn, message) in bad:
            matcher.bad(fn, encoding.strtolocal(message))

        status = scmutil.status(
            modified=modified,
            added=added,
            removed=removed,
            deleted=deleted,
            unknown=unknown,
            ignored=ignored,
            clean=clean,
        )
        return (lookup, status)

    def status(self, match, subrepos, ignored, clean, unknown):
        """Determine the status of the working copy relative to the
        dirstate and return a pair of (unsure, status), where status is of type
        scmutil.status and:

          unsure:
            files that might have been modified since the dirstate was
            written, but need to be read to be sure (size is the same
            but mtime differs)
          status.modified:
            files that have definitely been modified since the dirstate
            was written (different size or mode)
          status.clean:
            files that have definitely not been modified since the
            dirstate was written
        """
        listignored, listclean, listunknown = ignored, clean, unknown
        lookup, modified, added, unknown, ignored = [], [], [], [], []
        removed, deleted, clean = [], [], []

        dmap = self._map
        dmap.preload()

        use_rust = True

        allowed_matchers = (
            matchmod.alwaysmatcher,
            matchmod.differencematcher,
            matchmod.exactmatcher,
            matchmod.includematcher,
            matchmod.intersectionmatcher,
            matchmod.nevermatcher,
            matchmod.unionmatcher,
        )

        if rustmod is None:
            use_rust = False
        elif self._checkcase:
            # Case-insensitive filesystems are not handled yet
            use_rust = False
        elif subrepos:
            use_rust = False
        elif not isinstance(match, allowed_matchers):
            # Some matchers have yet to be implemented
            use_rust = False

        # Get the time from the filesystem so we can disambiguate files that
        # appear modified in the present or future.
        try:
            mtime_boundary = timestamp.get_fs_now(self._opener)
        except OSError:
            # In largefiles or readonly context
            mtime_boundary = None

        if use_rust:
            try:
                res = self._rust_status(
                    match, listclean, listignored, listunknown
                )
                return res + (mtime_boundary,)
            except rustmod.FallbackError:
                pass

        def noop(f):
            pass

        dcontains = dmap.__contains__
        dget = dmap.__getitem__
        ladd = lookup.append  # aka "unsure"
        madd = modified.append
        aadd = added.append
        uadd = unknown.append if listunknown else noop
        iadd = ignored.append if listignored else noop
        radd = removed.append
        dadd = deleted.append
        cadd = clean.append if listclean else noop
        mexact = match.exact
        dirignore = self._dirignore
        checkexec = self._checkexec
        checklink = self._checklink
        copymap = self._map.copymap

        # We need to do full walks when either
        # - we're listing all clean files, or
        # - match.traversedir does something, because match.traversedir should
        #   be called for every dir in the working dir
        full = listclean or match.traversedir is not None
        for fn, st in self.walk(
            match, subrepos, listunknown, listignored, full=full
        ).items():
            if not dcontains(fn):
                if (listignored or mexact(fn)) and dirignore(fn):
                    if listignored:
                        iadd(fn)
                else:
                    uadd(fn)
                continue

            t = dget(fn)
            mode = t.mode
            size = t.size

            if not st and t.tracked:
                dadd(fn)
            elif t.p2_info:
                madd(fn)
            elif t.added:
                aadd(fn)
            elif t.removed:
                radd(fn)
            elif t.tracked:
                if not checklink and t.has_fallback_symlink:
                    # If the file system does not support symlink, the mode
                    # might not be correctly stored in the dirstate, so do not
                    # trust it.
                    ladd(fn)
                elif not checkexec and t.has_fallback_exec:
                    # If the file system does not support exec bits, the mode
                    # might not be correctly stored in the dirstate, so do not
                    # trust it.
                    ladd(fn)
                elif (
                    size >= 0
                    and (
                        (size != st.st_size and size != st.st_size & _rangemask)
                        or ((mode ^ st.st_mode) & 0o100 and checkexec)
                    )
                    or fn in copymap
                ):
                    if stat.S_ISLNK(st.st_mode) and size != st.st_size:
                        # issue6456: Size returned may be longer due to
                        # encryption on EXT-4 fscrypt, undecided.
                        ladd(fn)
                    else:
                        madd(fn)
                elif not t.mtime_likely_equal_to(timestamp.mtime_of(st)):
                    # There might be a change in the future if for example the
                    # internal clock is off, but this is a case where the issues
                    # the user would face would be a lot worse and there is
                    # nothing we can really do.
                    ladd(fn)
                elif listclean:
                    cadd(fn)
        status = scmutil.status(
            modified, added, removed, deleted, unknown, ignored, clean
        )
        return (lookup, status, mtime_boundary)

    def matches(self, match):
        """
        return files in the dirstate (in whatever state) filtered by match
        """
        dmap = self._map
        if rustmod is not None:
            dmap = self._map._map

        if match.always():
            return dmap.keys()
        files = match.files()
        if match.isexact():
            # fast path -- filter the other way around, since typically files is
            # much smaller than dmap
            return [f for f in files if f in dmap]
        if match.prefix() and all(fn in dmap for fn in files):
            # fast path -- all the values are known to be files, so just return
            # that
            return list(files)
        return [f for f in dmap if match(f)]

    def _actualfilename(self, tr):
        if tr:
            return self._pendingfilename
        else:
            return self._filename

    def data_backup_filename(self, backupname):
        if not self._use_dirstate_v2:
            return None
        return backupname + b'.v2-data'

    def _new_backup_data_filename(self, backupname):
        """return a filename to backup a data-file or None"""
        if not self._use_dirstate_v2:
            return None
        if self._map.docket.uuid is None:
            # not created yet, nothing to backup
            return None
        data_filename = self._map.docket.data_filename()
        return data_filename, self.data_backup_filename(backupname)

    def backup_data_file(self, backupname):
        if not self._use_dirstate_v2:
            return None
        docket = docketmod.DirstateDocket.parse(
            self._opener.read(backupname),
            self._nodeconstants,
        )
        return self.data_backup_filename(backupname), docket.data_filename()

    def savebackup(self, tr, backupname):
        '''Save current dirstate into backup file'''
        filename = self._actualfilename(tr)
        assert backupname != filename

        # use '_writedirstate' instead of 'write' to write changes certainly,
        # because the latter omits writing out if transaction is running.
        # output file will be used to create backup of dirstate at this point.
        if self._dirty or not self._opener.exists(filename):
            self._writedirstate(
                tr,
                self._opener(filename, b"w", atomictemp=True, checkambig=True),
            )

        if tr:
            # ensure that subsequent tr.writepending returns True for
            # changes written out above, even if dirstate is never
            # changed after this
            tr.addfilegenerator(
                b'dirstate-1-main',
                (self._filename,),
                lambda f: self._writedirstate(tr, f),
                location=b'plain',
                post_finalize=True,
            )

            # ensure that pending file written above is unlinked at
            # failure, even if tr.writepending isn't invoked until the
            # end of this transaction
            tr.registertmp(filename, location=b'plain')

        self._opener.tryunlink(backupname)
        # hardlink backup is okay because _writedirstate is always called
        # with an "atomictemp=True" file.
        util.copyfile(
            self._opener.join(filename),
            self._opener.join(backupname),
            hardlink=True,
        )
        data_pair = self._new_backup_data_filename(backupname)
        if data_pair is not None:
            data_filename, bck_data_filename = data_pair
            util.copyfile(
                self._opener.join(data_filename),
                self._opener.join(bck_data_filename),
                hardlink=True,
            )
            if tr is not None:
                # ensure that pending file written above is unlinked at
                # failure, even if tr.writepending isn't invoked until the
                # end of this transaction
                tr.registertmp(bck_data_filename, location=b'plain')

    def restorebackup(self, tr, backupname):
        '''Restore dirstate by backup file'''
        # this "invalidate()" prevents "wlock.release()" from writing
        # changes of dirstate out after restoring from backup file
        self.invalidate()
        o = self._opener
        if not o.exists(backupname):
            # there was no file backup, delete existing files
            filename = self._actualfilename(tr)
            data_file = None
            if self._use_dirstate_v2 and self._map.docket.uuid is not None:
                data_file = self._map.docket.data_filename()
            if o.exists(filename):
                o.unlink(filename)
            if data_file is not None and o.exists(data_file):
                o.unlink(data_file)
            return
        filename = self._actualfilename(tr)
        data_pair = self.backup_data_file(backupname)
        if o.exists(filename) and util.samefile(
            o.join(backupname), o.join(filename)
        ):
            o.unlink(backupname)
        else:
            o.rename(backupname, filename, checkambig=True)

        if data_pair is not None:
            data_backup, target = data_pair
            if o.exists(target) and util.samefile(
                o.join(data_backup), o.join(target)
            ):
                o.unlink(data_backup)
            else:
                o.rename(data_backup, target, checkambig=True)

    def clearbackup(self, tr, backupname):
        '''Clear backup file'''
        o = self._opener
        if o.exists(backupname):
            data_backup = self.backup_data_file(backupname)
            o.unlink(backupname)
            if data_backup is not None:
                o.unlink(data_backup[0])

    def verify(self, m1, m2):
        """check the dirstate content again the parent manifest and yield errors"""
        missing_from_p1 = b"%s in state %s, but not in manifest1\n"
        unexpected_in_p1 = b"%s in state %s, but also in manifest1\n"
        missing_from_ps = b"%s in state %s, but not in either manifest\n"
        missing_from_ds = b"%s in manifest1, but listed as state %s\n"
        for f, entry in self.items():
            state = entry.state
            if state in b"nr" and f not in m1:
                yield (missing_from_p1, f, state)
            if state in b"a" and f in m1:
                yield (unexpected_in_p1, f, state)
            if state in b"m" and f not in m1 and f not in m2:
                yield (missing_from_ps, f, state)
        for f in m1:
            state = self.get_entry(f).state
            if state not in b"nrm":
                yield (missing_from_ds, f, state)
