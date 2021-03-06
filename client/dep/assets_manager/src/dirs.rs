use crate::{
    source::{DirEntry, Source},
    Asset, AssetCache, BoxedError, Compound, Error, Handle, SharedString,
};

use std::{fmt, io, marker::PhantomData};

pub trait DirLoadable: Compound {
    /// Returns the ids of the assets contained in the directory given by `id`.
    ///
    /// Note that the order of the returned ids is not kept, and that redundant
    /// ids are removed.
    fn select_ids<S: Source + ?Sized>(source: &S, id: &str) -> io::Result<Vec<SharedString>>;
}

impl<A> DirLoadable for A
where
    A: Asset,
{
    #[inline]
    fn select_ids<S: Source + ?Sized>(source: &S, id: &str) -> io::Result<Vec<SharedString>> {
        fn inner<S: Source + ?Sized>(
            source: &S,
            id: &str,
            extensions: &[&str],
        ) -> io::Result<Vec<SharedString>> {
            let mut ids = Vec::new();

            // Select all files with an extension valid for type `A`
            log::info!("read_dir select_ids");
            source.read_dir(id, &mut |entry| {
                if let DirEntry::File(id, ext) = entry {
                    if extensions.contains(&ext) {
                        ids.push(id.into());
                    }
                }
            })?;

            Ok(ids)
        }

        inner(source, id, A::EXTENSIONS)
    }
}

/// Stores ids in a directory containing assets of type `A`
pub(crate) struct CachedDir<A> {
    ids: Vec<SharedString>,
    _marker: PhantomData<A>,
}

impl<A> Compound for CachedDir<A>
where
    A: DirLoadable,
{
    fn load<S: Source + ?Sized>(cache: &AssetCache<S>, id: &str) -> Result<Self, BoxedError> {
        let mut ids =
            A::select_ids(cache.source(), id).map_err(|err| Error::from_io(id.into(), err))?;

        // Remove duplicated entries
        ids.sort_unstable();
        ids.dedup();

        Ok(CachedDir {
            ids,
            _marker: PhantomData,
        })
    }
}

impl<A: DirLoadable> crate::asset::NotHotReloaded for CachedDir<A> {}

impl<A> fmt::Debug for CachedDir<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.ids.fmt(f)
    }
}
enum DirHandleInner<'a, A> {
    Simple(Handle<'a, CachedDir<A>>),
}

impl<A> Clone for DirHandleInner<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A> Copy for DirHandleInner<'_, A> {}

impl<'a, A> DirHandleInner<'a, A>
where
    A: DirLoadable,
{
    #[inline]
    fn id(self) -> &'a str {
        match self {
            Self::Simple(handle) => handle.id(),
        }
    }

    #[inline]
    fn ids(self) -> &'a [SharedString] {
        match self {
            Self::Simple(handle) => &handle.get().ids,
        }
    }
}

/// A handle on a asset directory.
///
/// This type provides methods to access assets within a directory.
pub struct DirHandle<'a, A, S: ?Sized> {
    inner: DirHandleInner<'a, A>,
    cache: &'a AssetCache<S>,
}

impl<'a, A, S> DirHandle<'a, A, S>
where
    A: DirLoadable,
    S: ?Sized,
{
    #[inline]
    pub(crate) fn new(handle: Handle<'a, CachedDir<A>>, cache: &'a AssetCache<S>) -> Self {
        let inner = DirHandleInner::Simple(handle);
        DirHandle { inner, cache }
    }


    /// The id of the directory handle.
    #[inline]
    pub fn id(self) -> &'a str {
        self.inner.id()
    }

    /// Returns an iterator over the ids of the assets in the directory.
    #[inline]
    pub fn ids(self) -> impl ExactSizeIterator<Item = &'a str> {
        self.inner.ids().iter().map(|id| &**id)
    }

    /// Returns an iterator over the assets in the directory.
    ///
    /// This fonction does not do any I/O and assets that previously failed to
    /// load are ignored.
    #[inline]
    pub fn iter_cached(self) -> impl Iterator<Item = Handle<'a, A>> {
        self.inner
            .ids()
            .iter()
            .filter_map(move |id| self.cache.get_cached(&**id))
    }
}

impl<'a, A, S> DirHandle<'a, A, S>
where
    A: DirLoadable,
    S: Source + ?Sized,
{
    /// Returns an iterator over the assets in the directory.
    ///
    /// This function will happily try to load all assets, even if an error
    /// occured the last time it was tried.
    #[inline]
    pub fn iter(self) -> impl ExactSizeIterator<Item = Result<Handle<'a, A>, Error>> {
        self.inner
            .ids()
            .iter()
            .map(move |id| self.cache.load(&**id))
    }
}

impl<A, S: ?Sized> Clone for DirHandle<'_, A, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A, S: ?Sized> Copy for DirHandle<'_, A, S> {}

impl<A, S> fmt::Debug for DirHandle<'_, A, S>
where
    A: DirLoadable,
    S: ?Sized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirHandle")
            .field("ids", &self.inner.ids())
            .finish()
    }
}
