# dirstatemap.py
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.


from .i18n import _

from . import (
    error,
    pathutil,
    policy,
    txnutil,
    util,
)

from .dirstateutils import (
    docket as docketmod,
    v2,
)

parsers = policy.importmod('parsers')
rustmod = policy.importrust('dirstate')

propertycache = util.propertycache

if rustmod is None:
    DirstateItem = parsers.DirstateItem
else:
    DirstateItem = rustmod.DirstateItem

rangemask = 0x7FFFFFFF


class _dirstatemapcommon:
    """
    Methods that are identical for both implementations of the dirstatemap
    class, with and without Rust extensions enabled.
    """

    # please pytype

    _map = None
    copymap = None

    def __init__(self, ui, opener, root, nodeconstants, use_dirstate_v2):
        self._use_dirstate_v2 = use_dirstate_v2
        self._nodeconstants = nodeconstants
        self._ui = ui
        self._opener = opener
        self._root = root
        self._filename = b'dirstate'
        self._nodelen = 20  # Also update Rust code when changing this!
        self._parents = None
        self._dirtyparents = False
        self._docket = None

        # for consistent view between _pl() and _read() invocations
        self._pendingmode = None

    def preload(self):
        """Loads the underlying data, if it's not already loaded"""
        self._map

    def get(self, key, default=None):
        return self._map.get(key, default)

    def __len__(self):
        return len(self._map)

    def __iter__(self):
        return iter(self._map)

    def __contains__(self, key):
        return key in self._map

    def __getitem__(self, item):
        return self._map[item]

    ### disk interaction

    def _opendirstatefile(self):
        fp, mode = txnutil.trypending(self._root, self._opener, self._filename)
        if self._pendingmode is not None and self._pendingmode != mode:
            fp.close()
            raise error.Abort(
                _(b'working directory state may be changed parallelly')
            )
        self._pendingmode = mode
        return fp

    def _readdirstatefile(self, size=-1):
        try:
            with self._opendirstatefile() as fp:
                return fp.read(size)
        except FileNotFoundError:
            # File doesn't exist, so the current state is empty
            return b''

    @property
    def docket(self):
        if not self._docket:
            if not self._use_dirstate_v2:
                raise error.ProgrammingError(
                    b'dirstate only has a docket in v2 format'
                )
            self._docket = docketmod.DirstateDocket.parse(
                self._readdirstatefile(), self._nodeconstants
            )
        return self._docket

    def write_v2_no_append(self, tr, st, meta, packed):
        old_docket = self.docket
        new_docket = docketmod.DirstateDocket.with_new_uuid(
            self.parents(), len(packed), meta
        )
        if old_docket.uuid == new_docket.uuid:
            raise error.ProgrammingError(b'dirstate docket name collision')
        data_filename = new_docket.data_filename()
        self._opener.write(data_filename, packed)
        # Write the new docket after the new data file has been
        # written. Because `st` was opened with `atomictemp=True`,
        # the actual `.hg/dirstate` file is only affected on close.
        st.write(new_docket.serialize())
        st.close()
        # Remove the old data file after the new docket pointing to
        # the new data file was written.
        if old_docket.uuid:
            data_filename = old_docket.data_filename()
            unlink = lambda _tr=None: self._opener.unlink(data_filename)
            if tr:
                category = b"dirstate-v2-clean-" + old_docket.uuid
                tr.addpostclose(category, unlink)
            else:
                unlink()
        self._docket = new_docket

    ### reading/setting parents

    def parents(self):
        if not self._parents:
            if self._use_dirstate_v2:
                self._parents = self.docket.parents
            else:
                read_len = self._nodelen * 2
                st = self._readdirstatefile(read_len)
                l = len(st)
                if l == read_len:
                    self._parents = (
                        st[: self._nodelen],
                        st[self._nodelen : 2 * self._nodelen],
                    )
                elif l == 0:
                    self._parents = (
                        self._nodeconstants.nullid,
                        self._nodeconstants.nullid,
                    )
                else:
                    raise error.Abort(
                        _(b'working directory state appears damaged!')
                    )

        return self._parents


class dirstatemap(_dirstatemapcommon):
    """Map encapsulating the dirstate's contents.

    The dirstate contains the following state:

    - `identity` is the identity of the dirstate file, which can be used to
      detect when changes have occurred to the dirstate file.

    - `parents` is a pair containing the parents of the working copy. The
      parents are updated by calling `setparents`.

    - the state map maps filenames to tuples of (state, mode, size, mtime),
      where state is a single character representing 'normal', 'added',
      'removed', or 'merged'. It is read by treating the dirstate as a
      dict.  File state is updated by calling various methods (see each
      documentation for details):

      - `reset_state`,
      - `set_tracked`
      - `set_untracked`
      - `set_clean`
      - `set_possibly_dirty`

    - `copymap` maps destination filenames to their source filename.

    The dirstate also provides the following views onto the state:

    - `filefoldmap` is a dict mapping normalized filenames to the denormalized
      form that they appear as in the dirstate.

    - `dirfoldmap` is a dict mapping normalized directory names to the
      denormalized form that they appear as in the dirstate.
    """

    ### Core data storage and access

    @propertycache
    def _map(self):
        self._map = {}
        self.read()
        return self._map

    @propertycache
    def copymap(self):
        self.copymap = {}
        self._map
        return self.copymap

    def clear(self):
        self._map.clear()
        self.copymap.clear()
        self.setparents(self._nodeconstants.nullid, self._nodeconstants.nullid)
        util.clearcachedproperty(self, b"_dirs")
        util.clearcachedproperty(self, b"_alldirs")
        util.clearcachedproperty(self, b"filefoldmap")
        util.clearcachedproperty(self, b"dirfoldmap")

    def items(self):
        return self._map.items()

    # forward for python2,3 compat
    iteritems = items

    def debug_iter(self, all):
        """
        Return an iterator of (filename, state, mode, size, mtime) tuples

        `all` is unused when Rust is not enabled
        """
        for (filename, item) in self.items():
            yield (filename, item.state, item.mode, item.size, item.mtime)

    def keys(self):
        return self._map.keys()

    ### reading/setting parents

    def setparents(self, p1, p2, fold_p2=False):
        self._parents = (p1, p2)
        self._dirtyparents = True
        copies = {}
        if fold_p2:
            for f, s in self._map.items():
                # Discard "merged" markers when moving away from a merge state
                if s.p2_info:
                    source = self.copymap.pop(f, None)
                    if source:
                        copies[f] = source
                    s.drop_merge_data()
        return copies

    ### disk interaction

    def read(self):
        # ignore HG_PENDING because identity is used only for writing
        self.identity = util.filestat.frompath(
            self._opener.join(self._filename)
        )

        if self._use_dirstate_v2:
            if not self.docket.uuid:
                return
            st = self._opener.read(self.docket.data_filename())
        else:
            st = self._readdirstatefile()

        if not st:
            return

        # TODO: adjust this estimate for dirstate-v2
        if util.safehasattr(parsers, b'dict_new_presized'):
            # Make an estimate of the number of files in the dirstate based on
            # its size. This trades wasting some memory for avoiding costly
            # resizes. Each entry have a prefix of 17 bytes followed by one or
            # two path names. Studies on various large-scale real-world repositories
            # found 54 bytes a reasonable upper limit for the average path names.
            # Copy entries are ignored for the sake of this estimate.
            self._map = parsers.dict_new_presized(len(st) // 71)

        # Python's garbage collector triggers a GC each time a certain number
        # of container objects (the number being defined by
        # gc.get_threshold()) are allocated. parse_dirstate creates a tuple
        # for each file in the dirstate. The C version then immediately marks
        # them as not to be tracked by the collector. However, this has no
        # effect on when GCs are triggered, only on what objects the GC looks
        # into. This means that O(number of files) GCs are unavoidable.
        # Depending on when in the process's lifetime the dirstate is parsed,
        # this can get very expensive. As a workaround, disable GC while
        # parsing the dirstate.
        #
        # (we cannot decorate the function directly since it is in a C module)
        if self._use_dirstate_v2:
            p = self.docket.parents
            meta = self.docket.tree_metadata
            parse_dirstate = util.nogc(v2.parse_dirstate)
            parse_dirstate(self._map, self.copymap, st, meta)
        else:
            parse_dirstate = util.nogc(parsers.parse_dirstate)
            p = parse_dirstate(self._map, self.copymap, st)
        if not self._dirtyparents:
            self.setparents(*p)

        # Avoid excess attribute lookups by fast pathing certain checks
        self.__contains__ = self._map.__contains__
        self.__getitem__ = self._map.__getitem__
        self.get = self._map.get

    def write(self, tr, st):
        if self._use_dirstate_v2:
            packed, meta = v2.pack_dirstate(self._map, self.copymap)
            self.write_v2_no_append(tr, st, meta, packed)
        else:
            packed = parsers.pack_dirstate(
                self._map, self.copymap, self.parents()
            )
            st.write(packed)
            st.close()
        self._dirtyparents = False

    @propertycache
    def identity(self):
        self._map
        return self.identity

    ### code related to maintaining and accessing "extra" property
    # (e.g. "has_dir")

    def _dirs_incr(self, filename, old_entry=None):
        """increment the dirstate counter if applicable"""
        if (
            old_entry is None or old_entry.removed
        ) and "_dirs" in self.__dict__:
            self._dirs.addpath(filename)
        if old_entry is None and "_alldirs" in self.__dict__:
            self._alldirs.addpath(filename)

    def _dirs_decr(self, filename, old_entry=None, remove_variant=False):
        """decrement the dirstate counter if applicable"""
        if old_entry is not None:
            if "_dirs" in self.__dict__ and not old_entry.removed:
                self._dirs.delpath(filename)
            if "_alldirs" in self.__dict__ and not remove_variant:
                self._alldirs.delpath(filename)
        elif remove_variant and "_alldirs" in self.__dict__:
            self._alldirs.addpath(filename)
        if "filefoldmap" in self.__dict__:
            normed = util.normcase(filename)
            self.filefoldmap.pop(normed, None)

    @propertycache
    def filefoldmap(self):
        """Returns a dictionary mapping normalized case paths to their
        non-normalized versions.
        """
        try:
            makefilefoldmap = parsers.make_file_foldmap
        except AttributeError:
            pass
        else:
            return makefilefoldmap(
                self._map, util.normcasespec, util.normcasefallback
            )

        f = {}
        normcase = util.normcase
        for name, s in self._map.items():
            if not s.removed:
                f[normcase(name)] = name
        f[b'.'] = b'.'  # prevents useless util.fspath() invocation
        return f

    @propertycache
    def dirfoldmap(self):
        f = {}
        normcase = util.normcase
        for name in self._dirs:
            f[normcase(name)] = name
        return f

    def hastrackeddir(self, d):
        """
        Returns True if the dirstate contains a tracked (not removed) file
        in this directory.
        """
        return d in self._dirs

    def hasdir(self, d):
        """
        Returns True if the dirstate contains a file (tracked or removed)
        in this directory.
        """
        return d in self._alldirs

    @propertycache
    def _dirs(self):
        return pathutil.dirs(self._map, only_tracked=True)

    @propertycache
    def _alldirs(self):
        return pathutil.dirs(self._map)

    ### code related to manipulation of entries and copy-sources

    def reset_state(
        self,
        filename,
        wc_tracked=False,
        p1_tracked=False,
        p2_info=False,
        has_meaningful_mtime=True,
        parentfiledata=None,
    ):
        """Set a entry to a given state, diregarding all previous state

        This is to be used by the part of the dirstate API dedicated to
        adjusting the dirstate after a update/merge.

        note: calling this might result to no entry existing at all if the
        dirstate map does not see any point at having one for this file
        anymore.
        """
        # copy information are now outdated
        # (maybe new information should be in directly passed to this function)
        self.copymap.pop(filename, None)

        if not (p1_tracked or p2_info or wc_tracked):
            old_entry = self._map.get(filename)
            self._drop_entry(filename)
            self._dirs_decr(filename, old_entry=old_entry)
            return

        old_entry = self._map.get(filename)
        self._dirs_incr(filename, old_entry)
        entry = DirstateItem(
            wc_tracked=wc_tracked,
            p1_tracked=p1_tracked,
            p2_info=p2_info,
            has_meaningful_mtime=has_meaningful_mtime,
            parentfiledata=parentfiledata,
        )
        self._map[filename] = entry

    def set_tracked(self, filename):
        new = False
        entry = self.get(filename)
        if entry is None:
            self._dirs_incr(filename)
            entry = DirstateItem(
                wc_tracked=True,
            )

            self._map[filename] = entry
            new = True
        elif not entry.tracked:
            self._dirs_incr(filename, entry)
            entry.set_tracked()
            self._refresh_entry(filename, entry)
            new = True
        else:
            # XXX This is probably overkill for more case, but we need this to
            # fully replace the `normallookup` call with `set_tracked` one.
            # Consider smoothing this in the future.
            entry.set_possibly_dirty()
            self._refresh_entry(filename, entry)
        return new

    def set_untracked(self, f):
        """Mark a file as no longer tracked in the dirstate map"""
        entry = self.get(f)
        if entry is None:
            return False
        else:
            self._dirs_decr(f, old_entry=entry, remove_variant=not entry.added)
            if not entry.p2_info:
                self.copymap.pop(f, None)
            entry.set_untracked()
            self._refresh_entry(f, entry)
            return True

    def set_clean(self, filename, mode, size, mtime):
        """mark a file as back to a clean state"""
        entry = self[filename]
        size = size & rangemask
        entry.set_clean(mode, size, mtime)
        self._refresh_entry(filename, entry)
        self.copymap.pop(filename, None)

    def set_possibly_dirty(self, filename):
        """record that the current state of the file on disk is unknown"""
        entry = self[filename]
        entry.set_possibly_dirty()
        self._refresh_entry(filename, entry)

    def _refresh_entry(self, f, entry):
        """record updated state of an entry"""
        if not entry.any_tracked:
            self._map.pop(f, None)

    def _drop_entry(self, f):
        """remove any entry for file f

        This should also drop associated copy information

        The fact we actually need to drop it is the responsability of the caller"""
        self._map.pop(f, None)
        self.copymap.pop(f, None)


if rustmod is not None:

    class dirstatemap(_dirstatemapcommon):

        ### Core data storage and access

        @propertycache
        def _map(self):
            """
            Fills the Dirstatemap when called.
            """
            # ignore HG_PENDING because identity is used only for writing
            self.identity = util.filestat.frompath(
                self._opener.join(self._filename)
            )

            if self._use_dirstate_v2:
                if self.docket.uuid:
                    # TODO: use mmap when possible
                    data = self._opener.read(self.docket.data_filename())
                else:
                    data = b''
                self._map = rustmod.DirstateMap.new_v2(
                    data, self.docket.data_size, self.docket.tree_metadata
                )
                parents = self.docket.parents
            else:
                self._map, parents = rustmod.DirstateMap.new_v1(
                    self._readdirstatefile()
                )

            if parents and not self._dirtyparents:
                self.setparents(*parents)

            self.__contains__ = self._map.__contains__
            self.__getitem__ = self._map.__getitem__
            self.get = self._map.get
            return self._map

        @property
        def copymap(self):
            return self._map.copymap()

        def debug_iter(self, all):
            """
            Return an iterator of (filename, state, mode, size, mtime) tuples

            `all`: also include with `state == b' '` dirstate tree nodes that
            don't have an associated `DirstateItem`.

            """
            return self._map.debug_iter(all)

        def clear(self):
            self._map.clear()
            self.setparents(
                self._nodeconstants.nullid, self._nodeconstants.nullid
            )
            util.clearcachedproperty(self, b"_dirs")
            util.clearcachedproperty(self, b"_alldirs")
            util.clearcachedproperty(self, b"dirfoldmap")

        def items(self):
            return self._map.items()

        # forward for python2,3 compat
        iteritems = items

        def keys(self):
            return iter(self._map)

        ### reading/setting parents

        def setparents(self, p1, p2, fold_p2=False):
            self._parents = (p1, p2)
            self._dirtyparents = True
            copies = {}
            if fold_p2:
                copies = self._map.setparents_fixup()
            return copies

        ### disk interaction

        @propertycache
        def identity(self):
            self._map
            return self.identity

        def write(self, tr, st):
            if not self._use_dirstate_v2:
                p1, p2 = self.parents()
                packed = self._map.write_v1(p1, p2)
                st.write(packed)
                st.close()
                self._dirtyparents = False
                return

            # We can only append to an existing data file if there is one
            can_append = self.docket.uuid is not None
            packed, meta, append = self._map.write_v2(can_append)
            if append:
                docket = self.docket
                data_filename = docket.data_filename()
                with self._opener(data_filename, b'r+b') as fp:
                    fp.seek(docket.data_size)
                    assert fp.tell() == docket.data_size
                    written = fp.write(packed)
                    if written is not None:  # py2 may return None
                        assert written == len(packed), (written, len(packed))
                docket.data_size += len(packed)
                docket.parents = self.parents()
                docket.tree_metadata = meta
                st.write(docket.serialize())
                st.close()
            else:
                self.write_v2_no_append(tr, st, meta, packed)
            # Reload from the newly-written file
            util.clearcachedproperty(self, b"_map")
            self._dirtyparents = False

        ### code related to maintaining and accessing "extra" property
        # (e.g. "has_dir")

        @propertycache
        def filefoldmap(self):
            """Returns a dictionary mapping normalized case paths to their
            non-normalized versions.
            """
            return self._map.filefoldmapasdict()

        def hastrackeddir(self, d):
            return self._map.hastrackeddir(d)

        def hasdir(self, d):
            return self._map.hasdir(d)

        @propertycache
        def dirfoldmap(self):
            f = {}
            normcase = util.normcase
            for name in self._map.tracked_dirs():
                f[normcase(name)] = name
            return f

        ### code related to manipulation of entries and copy-sources

        def set_tracked(self, f):
            return self._map.set_tracked(f)

        def set_untracked(self, f):
            return self._map.set_untracked(f)

        def set_clean(self, filename, mode, size, mtime):
            self._map.set_clean(filename, mode, size, mtime)

        def set_possibly_dirty(self, f):
            self._map.set_possibly_dirty(f)

        def reset_state(
            self,
            filename,
            wc_tracked=False,
            p1_tracked=False,
            p2_info=False,
            has_meaningful_mtime=True,
            parentfiledata=None,
        ):
            return self._map.reset_state(
                filename,
                wc_tracked,
                p1_tracked,
                p2_info,
                has_meaningful_mtime,
                parentfiledata,
            )
