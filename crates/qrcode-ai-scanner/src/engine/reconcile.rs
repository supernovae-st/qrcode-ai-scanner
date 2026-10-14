//! Collect, then reconcile (07-pipeline property 6 § Reconciliation).
//!
//! A walk collects reads: every engine hit of every attempt, upgraded by the
//! scanner's own parses (the re-read), the candidate parses of grids rqrr
//! detected but did not decode (no engine), and the S5 rescue. Identical
//! reads fold into one. Refusals are records of refused streams — one per
//! stream and names, wherever and however often it was refused; what they
//! withhold, and what protects a read from them, is evaluated over the
//! whole walk — so retroactively — whenever the walk asks what stands.
//! [`ReadStore::reconcile`] turns what stands into the distinct contents
//! the scanner can tell apart — a function of the read set, never of the
//! order the reads arrived in:
//!
//! 1. a candidate parse joins only when it contains an engine read of its
//!    bytes with an agreeing account;
//! 2. per (symbology, raw): located and no-ECI reads with an observed FNC1
//!    position form one detection per (evidence, position); a signalled
//!    read joins the one located detection of its position that contains
//!    it, or the group's signalled detection when there is no located one;
//!    reads with unobserved evidence, then reads with an unobserved
//!    position, join the one agreeing detection that contains them, else
//!    form one detection per published identity;
//! 3. a detection no engine read belongs to is not published; at most
//!    [`MAX_DETECTIONS`], QR family first, then by earliest read.
//!
//! What stands is first weighed by the acceptance (07-pipeline
//! § Acceptance, [`admitted`]): a read of a symbology the readers invent
//! from noise waits for a read of its bytes on a plane of independent
//! scale, and a QR content whose every measured correction margin is
//! negative is refused, its reads of unknown margin with it. A held read
//! stays in the walk — it protects agreeing reads and keeps re-derivation
//! running — but it neither settles a stage nor stops S5, and it is never
//! published.

use super::content::{ContentKey, DoubleByteSpan, EciEvidence, EngineReading, Fnc1, Segments};
use super::{MaskedStream, Refusal};
use crate::report::{EcLevel, EngineKind, Point, Symbology};
use crate::rescue::Margin;

/// Distinct reads a walk reconciles; later distinct reads are counted.
pub(crate) const MAX_DISTINCT_READS: usize = 64;

/// Detections a report publishes — a tile of thousands of micro-QRs is a
/// CPU/report amplifier, not a use case.
pub(crate) const MAX_DETECTIONS: usize = 16;

/// Refused streams a walk remembers for the whole scan: one record per
/// stream and pair of names (rqrr's decode names both; the re-read and a
/// re-derived grid name the port's reading only), wherever and however
/// often the stream was refused.
pub(crate) const MAX_REFUSALS: usize = 16;

/// Records past [`MAX_REFUSALS`]: each withholds what was collected up to
/// the latest attempt that refused its stream, then lapses; past them the
/// walk fails closed. The opposite-background walk receives them all.
const MAX_OVERFLOW: usize = MAX_REFUSALS;

/// Distinct places a folded read or a refusal record remembers — distinct
/// symbols: a place whose centre lies inside a recorded one is the same
/// symbol ([`recorded`]). A refusal withholds a read only when every place
/// it was read at lies inside the refused symbol; a record refused at more
/// places withholds wherever its names match.
const MAX_PLACES: usize = 8;

/// Where a read sits in the walk: its attempt, its source within the
/// attempt ([`source`]) and its index in that source's output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Rank {
    /// Attempt index in walk order.
    pub(crate) attempt: u32,
    /// The source within the attempt.
    pub(crate) source: u8,
    /// Index within the source's output.
    pub(crate) index: u32,
}

/// The sources a [`Rank`] orders, within an attempt.
pub(crate) mod source {
    /// rxing's reads.
    pub(crate) const RXING: u8 = 0;
    /// rqrr's reads.
    pub(crate) const RQRR: u8 = 1;
    /// Candidate parses (grids rqrr detected, re-derived by the scanner).
    pub(crate) const CANDIDATE: u8 = 2;
    /// The S5 rescue.
    pub(crate) const RESCUE: u8 = 3;
}

/// What a read measured of its symbol (original image space).
#[derive(Debug, Clone, Default)]
pub(crate) struct Geometry {
    /// Symbol corners (rqrr, candidates, the rescue) and whether the
    /// attempt that measured them inverted the symbol photometrically.
    pub(crate) corners: Option<([Point; 4], bool)>,
    /// The verified stream with the version, EC level and mask read from
    /// it — the synthetic-UEC input, adopted as a whole.
    pub(crate) stream: Option<(MaskedStream, u8, EcLevel, u8)>,
    /// An EC level stated without a stream (rxing's port).
    pub(crate) ec: Option<EcLevel>,
}

/// One read of the walk.
#[derive(Debug, Clone)]
pub(crate) struct Read {
    /// The symbology it was read as.
    pub(crate) symbology: Symbology,
    /// Its bytes, ECI evidence and FNC1 position.
    pub(crate) key: ContentKey,
    /// The Kanji/Hanzi segments a parse of it observed.
    pub(crate) segments: Segments,
    /// The decoding engine's mode-aware reading (rule 1e).
    pub(crate) engine_reading: EngineReading,
    /// Engines that delivered it, each with its earliest rank; empty: a
    /// candidate parse.
    pub(crate) engines: Vec<(EngineKind, Rank)>,
    /// Its earliest rank.
    pub(crate) rank: Rank,
    /// The last attempt it was read in.
    pub(crate) last_attempt: u32,
    /// The corner points of the symbol it was first read at (internal).
    pub(crate) location: Option<[Point; 4]>,
    /// Every distinct symbol it was read at (at most [`MAX_PLACES`]).
    pub(crate) places: Vec<[Point; 4]>,
    /// What it measured.
    pub(crate) geometry: Geometry,
    /// The ranks of the reads its location and geometry parts come from.
    sources: Sources,
    /// The scanner's own parser read its stream.
    pub(crate) parsed: bool,
    /// The best correction margin a read folded into this one measured on
    /// its own received word (`Unknown`: none did).
    pub(crate) margin: Margin,
    /// Its symbology is one the readers invent from noise: it waits for a
    /// read of its bytes on a plane of independent scale (cleared when a
    /// read of it that needs none folds in).
    pub(crate) needs_confirmation: bool,
    /// The least and the greatest longest side of the planes it was read
    /// on (`None`: none recorded).
    pub(crate) scales: Option<(u32, u32)>,
}

/// Which read of a fold supplied each part: every part comes from the
/// earliest read that carries it.
#[derive(Debug, Clone, Copy)]
struct Sources {
    location: Rank,
    corners: Rank,
    stream: Rank,
    ec: Rank,
}

impl Read {
    /// A read by `engine` (`None`: a candidate parse) at `rank`.
    pub(crate) fn new(
        symbology: Symbology,
        key: ContentKey,
        engine: Option<EngineKind>,
        rank: Rank,
    ) -> Self {
        Self {
            symbology,
            key,
            segments: Segments::NotObserved,
            engine_reading: EngineReading::None,
            engines: engine.map(|engine| (engine, rank)).into_iter().collect(),
            rank,
            last_attempt: rank.attempt,
            location: None,
            places: Vec::new(),
            geometry: Geometry::default(),
            sources: Sources {
                location: rank,
                corners: rank,
                stream: rank,
                ec: rank,
            },
            parsed: false,
            margin: Margin::Unknown,
            needs_confirmation: false,
            scales: None,
        }
    }

    /// The read, made at `location`.
    pub(crate) fn at(mut self, location: Option<[Point; 4]>) -> Self {
        self.location = location;
        self.places = location.into_iter().collect();
        self
    }

    /// Same fold identity: identical in symbology, raw, FNC1, evidence,
    /// segments and engine reading.
    fn identical(&self, other: &Self) -> bool {
        self.symbology == other.symbology
            && self.key == other.key
            && self.segments == other.segments
            && self.engine_reading == other.engine_reading
    }

    /// Fold an identical read in: engines united (earliest rank each), the
    /// earliest rank, every symbol it was read at; the location and each
    /// geometry part from the earliest read that carries it.
    fn fold(&mut self, other: Self) {
        for (engine, rank) in other.engines {
            match self.engines.iter_mut().find(|(own, _)| *own == engine) {
                Some((_, own)) => *own = (*own).min(rank),
                None => self.engines.push((engine, rank)),
            }
        }
        self.rank = self.rank.min(other.rank);
        self.last_attempt = self.last_attempt.max(other.last_attempt);
        // one symbol read at every attempt scale is one place: the slots
        // stay for other symbols (a twin elsewhere)
        for place in other.places {
            if self.places.len() < MAX_PLACES && !recorded(&self.places, place) {
                self.places.push(place);
            }
        }
        let take = |own: bool, own_rank: Rank, theirs: bool, their_rank: Rank| {
            theirs && (!own || their_rank < own_rank)
        };
        let (own, theirs) = (self.sources, other.sources);
        if take(
            self.location.is_some(),
            own.location,
            other.location.is_some(),
            theirs.location,
        ) {
            self.location = other.location;
            self.sources.location = theirs.location;
        }
        if take(
            self.geometry.corners.is_some(),
            own.corners,
            other.geometry.corners.is_some(),
            theirs.corners,
        ) {
            self.geometry.corners = other.geometry.corners;
            self.sources.corners = theirs.corners;
        }
        if take(
            self.geometry.stream.is_some(),
            own.stream,
            other.geometry.stream.is_some(),
            theirs.stream,
        ) {
            self.geometry.stream = other.geometry.stream;
            self.sources.stream = theirs.stream;
        }
        if take(
            self.geometry.ec.is_some(),
            own.ec,
            other.geometry.ec.is_some(),
            theirs.ec,
        ) {
            self.geometry.ec = other.geometry.ec;
            self.sources.ec = theirs.ec;
        }
        self.parsed |= other.parsed;
        self.margin = self.margin.best(other.margin);
        self.needs_confirmation &= other.needs_confirmation;
        self.scales = match (self.scales, other.scales) {
            (Some((lo, hi)), Some((their_lo, their_hi))) => {
                Some((lo.min(their_lo), hi.max(their_hi)))
            }
            (scales, None) | (None, scales) => scales,
        };
    }

    /// An engine delivered it (it is no candidate parse).
    fn by_engine(&self) -> bool {
        !self.engines.is_empty()
    }
}

/// What a refusal does to one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cover {
    /// Nothing.
    Nothing,
    /// Withholds rqrr's part of it.
    Rqrr,
    /// Withholds all of it.
    Whole,
}

/// Whether `place` is a recorded symbol: its centre lies inside the box of
/// one of `places`.
fn recorded(places: &[[Point; 4]], place: [Point; 4]) -> bool {
    places.iter().any(|&own| inside(Some(place), Some(own)))
}

/// A refused stream as the walk remembers it: its names, the symbols it was
/// refused at, and the latest attempt that refused it.
#[derive(Debug, Clone)]
struct Refused {
    /// What rxing's port reports for the stream.
    port: Option<ContentKey>,
    /// The bytes rqrr decoded from it.
    rqrr: Option<Vec<u8>>,
    /// The distinct symbols it was refused at (at most [`MAX_PLACES`]);
    /// `None`: anywhere — refused without a place, or at more symbols.
    places: Option<Vec<[Point; 4]>>,
    /// The latest attempt that refused it.
    attempt: u32,
}

impl Refused {
    fn new(refusal: Refusal, attempt: u32) -> Self {
        Self {
            port: refusal.port,
            rqrr: refusal.rqrr,
            places: refusal.place.map(|place| vec![place]),
            attempt,
        }
    }

    /// The same names: the same refused stream, wherever it was refused.
    fn names(&self, refusal: &Refusal) -> bool {
        self.port == refusal.port && self.rqrr == refusal.rqrr
    }

    /// The stream refused again, at `place` in `attempt`.
    fn again(&mut self, place: Option<[Point; 4]>, attempt: u32) {
        self.attempt = self.attempt.max(attempt);
        let Some(place) = place else {
            self.places = None;
            return;
        };
        if let Some(places) = &mut self.places {
            if recorded(places, place) {
                return;
            }
            if places.len() < MAX_PLACES {
                places.push(place);
                return;
            }
        }
        // refused at more symbols than it remembers: anywhere (fail closed)
        self.places = None;
    }

    /// Whether a read made at `place` lies inside the refused symbol.
    fn holds(&self, place: [Point; 4]) -> bool {
        self.places
            .as_ref()
            .is_none_or(|places| recorded(places, place))
    }

    /// The refusals that record it again: one per place, or one without a
    /// place when it withholds anywhere.
    fn refusals(&self) -> Vec<Refusal> {
        let refusal = |place| Refusal {
            port: self.port.clone(),
            rqrr: self.rqrr.clone(),
            place,
        };
        match &self.places {
            Some(places) => places.iter().map(|&place| refusal(Some(place))).collect(),
            None => vec![refusal(None)],
        }
    }
}

/// What a refusal record withholds of a read: an unparsed QR Code read
/// lying inside the refused symbol — every place it was read at inside one
/// of the record's — whose account the port name matches (all of it), or
/// of rqrr's bytes (rqrr's part). A read or a record without a place never
/// fails the place test.
fn cover(refused: &Refused, read: &Read) -> Cover {
    if read.symbology != Symbology::QrCode || read.parsed {
        return Cover::Nothing;
    }
    if !read.places.iter().all(|&place| refused.holds(place)) {
        return Cover::Nothing;
    }
    if refused
        .port
        .as_ref()
        .is_some_and(|port| port.agrees(&read.key))
    {
        Cover::Whole
    } else if refused
        .rqrr
        .as_ref()
        .is_some_and(|bytes| *bytes == read.key.raw)
        && read.engines.iter().any(|(e, _)| *e == EngineKind::Rqrr)
    {
        Cover::Rqrr
    } else {
        Cover::Nothing
    }
}

/// What a walk hands the opposite-background walk: its refusal records
/// (those past capacity included) as refusals, whether it failed closed,
/// the parsed contents that protect reads, and the re-reads the scan
/// already spent.
#[derive(Debug, Clone, Default)]
pub(crate) struct Seed {
    /// Every refusal record of the walk, one refusal per place (one
    /// without a place when the record withholds anywhere).
    pub(crate) refusals: Vec<Refusal>,
    /// The walk failed closed: refused streams past every record.
    pub(crate) closed: bool,
    /// Contents a parse of the walk read: no refusal withholds a read they
    /// agree with.
    pub(crate) protection: Vec<(Symbology, ContentKey)>,
    /// Re-reads already spent by the scan.
    pub(crate) rereads: usize,
}

/// The reads of one walk.
#[derive(Debug, Default)]
pub(crate) struct ReadStore {
    reads: Vec<Read>,
    /// Distinct reads past [`MAX_DISTINCT_READS`]: counted, not reconciled.
    pub(crate) dropped: u32,
    /// Refused streams within capacity: they withhold across the whole walk.
    refusals: Vec<Refused>,
    /// Refused streams past capacity: each withholds what was read up to the
    /// latest attempt that refused it.
    overflow: Vec<Refused>,
    /// Refused streams past every record: the walk fails closed — every
    /// unparsed QR Code read collected up to this attempt is withheld.
    closed: Option<u32>,
    /// Another walk's parsed contents.
    protection: Vec<(Symbology, ContentKey)>,
}

impl ReadStore {
    /// A store that starts from another walk's refusals — recorded by the
    /// rule of [`ReadStore::refuse`], each withholding for this whole walk —
    /// its failing closed (for this whole walk too) and its protection.
    pub(crate) fn seeded(seed: Seed) -> Self {
        let mut store = Self {
            protection: seed.protection,
            closed: seed.closed.then_some(u32::MAX),
            ..Self::default()
        };
        for refusal in seed.refusals {
            store.refuse(refusal, u32::MAX);
        }
        store
    }

    /// Collect one read: folded into an identical read, or kept while
    /// capacity lasts (later distinct reads are counted).
    pub(crate) fn collect(&mut self, read: Read) {
        if let Some(own) = self.reads.iter_mut().find(|own| own.identical(&read)) {
            own.fold(read);
        } else if self.reads.len() < MAX_DISTINCT_READS {
            self.reads.push(read);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Record a refused stream found in `attempt`: the record of its names
    /// takes the place and the attempt; new names take a record while
    /// capacity lasts; past every record the walk fails closed up to
    /// `attempt`.
    pub(crate) fn refuse(&mut self, refusal: Refusal, attempt: u32) {
        if let Some(own) = self
            .refusals
            .iter_mut()
            .chain(self.overflow.iter_mut())
            .find(|own| own.names(&refusal))
        {
            own.again(refusal.place, attempt);
        } else if self.refusals.len() < MAX_REFUSALS {
            self.refusals.push(Refused::new(refusal, attempt));
        } else if self.overflow.len() < MAX_OVERFLOW {
            self.overflow.push(Refused::new(refusal, attempt));
        } else {
            self.closed = Some(self.closed.map_or(attempt, |closed| closed.max(attempt)));
        }
    }

    /// Whether a parsed read, a candidate parse or a seeded content of the
    /// same symbology agrees with the read: then no refusal withholds it.
    fn protected(&self, read: &Read) -> bool {
        self.reads
            .iter()
            .any(|p| p.parsed && p.symbology == read.symbology && p.key.agrees(&read.key))
            || self
                .protection
                .iter()
                .any(|(symbology, key)| *symbology == read.symbology && key.agrees(&read.key))
    }

    /// The read as it stands: `None` when withheld; rqrr's part removed when
    /// only that is withheld.
    fn standing(&self, read: &Read) -> Option<Read> {
        if self.protected(read) {
            return Some(read.clone());
        }
        if self
            .closed
            .is_some_and(|closed| read.last_attempt <= closed)
            && read.symbology == Symbology::QrCode
            && !read.parsed
        {
            return None;
        }
        let refusals = self.refusals.iter().chain(
            self.overflow
                .iter()
                .filter(|refused| read.last_attempt <= refused.attempt),
        );
        let mut rqrr_withheld = false;
        for refusal in refusals {
            match cover(refusal, read) {
                Cover::Whole => return None,
                Cover::Rqrr => rqrr_withheld = true,
                Cover::Nothing => {}
            }
        }
        if !rqrr_withheld {
            return Some(read.clone());
        }
        let mut read = read.clone();
        read.engines
            .retain(|(engine, _)| *engine != EngineKind::Rqrr);
        read.rank = read.engines.iter().map(|(_, rank)| *rank).min()?;
        Some(read)
    }

    /// Whether the recorded refusals withhold this read whole (what the
    /// re-read skips: it would be withheld anyway).
    pub(crate) fn withheld(&self, read: &Read) -> bool {
        self.standing(read).is_none()
    }

    /// Whether a standing read of the read's content (its symbology, an
    /// agreeing account) measured a margin of zero or more on its own word:
    /// a margin re-read of it would measure nothing the acceptance needs.
    pub(crate) fn has_margin(&self, read: &Read) -> bool {
        self.live().iter().any(|own| {
            own.symbology == read.symbology
                && own.key.agrees(&read.key)
                && own.margin.within_reserve()
        })
    }

    /// The reads that stand.
    fn live(&self) -> Vec<Read> {
        self.reads
            .iter()
            .filter_map(|read| self.standing(read))
            .collect()
    }

    /// The reads that stand and that the acceptance admits ([`admitted`]).
    fn accepted(&self) -> Vec<Read> {
        let live = self.live();
        live.iter()
            .filter(|read| admitted(&live, read))
            .cloned()
            .collect()
    }

    /// Whether standing reads of `symbology` and `raw` confirm each other
    /// ([`confirmed`]): what a confirmation decode waits for.
    pub(crate) fn confirmed(&self, symbology: Symbology, raw: &[u8]) -> bool {
        confirmed(&self.live(), symbology, raw)
    }

    /// Whether the walk publishes something — what the S5 trigger asks.
    pub(crate) fn found(&self) -> bool {
        !self.reconcile().is_empty()
    }

    /// Whether a QR Code read the scanner has not parsed stands — what
    /// re-deriving candidates waits for.
    pub(crate) fn unparsed_qr(&self) -> bool {
        self.live()
            .iter()
            .any(|read| read.by_engine() && read.symbology == Symbology::QrCode && !read.parsed)
    }

    /// Whether the walk may stop after a stage (07-pipeline property 2): an
    /// engine read stands that the acceptance admits, and not every such QR
    /// Code read only signals an ECI it could not locate. A signalled read
    /// a candidate parse contains with an agreeing account counts as
    /// located; other symbologies neither stop nor prolong the walk.
    pub(crate) fn settled(&self) -> bool {
        let live = self.live();
        let engine_reads: Vec<&Read> = live
            .iter()
            .filter(|read| read.by_engine() && admitted(&live, read))
            .collect();
        if engine_reads.is_empty() {
            return false;
        }
        let qr: Vec<&&Read> = engine_reads
            .iter()
            .filter(|read| read.symbology == Symbology::QrCode)
            .collect();
        qr.is_empty() || !qr.iter().all(|read| awaits_location(&live, read))
    }

    /// What the opposite-background walk starts with.
    pub(crate) fn seed(&self, rereads: usize) -> Seed {
        Seed {
            refusals: self
                .refusals
                .iter()
                .chain(&self.overflow)
                .flat_map(Refused::refusals)
                .collect(),
            closed: self.closed.is_some(),
            protection: self
                .reads
                .iter()
                .filter(|read| read.parsed)
                .map(|read| (read.symbology, read.key.clone()))
                .chain(self.protection.iter().cloned())
                .collect(),
            rereads,
        }
    }

    /// The distinct contents of the walk (module doc): of the reads the
    /// acceptance admits.
    pub(crate) fn reconcile(&self) -> Vec<Reconciled> {
        reconcile(self.accepted())
    }

    /// The refusal records kept, within capacity and past it.
    #[cfg(test)]
    pub(crate) fn records(&self) -> usize {
        self.refusals.len() + self.overflow.len()
    }
}

/// Whether the acceptance admits a standing read (07-pipeline
/// § Acceptance): it is no read of a symbology the readers invent from
/// noise still waiting for confirmation ([`confirmed`]), and its content —
/// the reads of its symbology whose account agrees with its own — is not
/// refused for its margins: a refused content has a read with a known
/// margin and every known margin negative, errors alone having taken each
/// measured word past the Annex B reserve. Reads of it with no margin
/// share the refusal (both engines sample the same damaged modules); one
/// read keeping a margin of zero or more admits them all.
fn admitted(live: &[Read], read: &Read) -> bool {
    if read.needs_confirmation && !confirmed(live, read.symbology, &read.key.raw) {
        return false;
    }
    let mut known = live
        .iter()
        .filter(|other| other.symbology == read.symbology && other.key.agrees(&read.key))
        .filter_map(|other| match other.margin {
            Margin::Known(margin) => Some(margin),
            Margin::Unknown => None,
        })
        .peekable();
    known.peek().is_none() || known.any(|margin| margin >= 0)
}

/// Whether reads of `symbology` and `raw` stand confirmed: one of them needs
/// no confirmation, or two were read on planes whose longest sides differ
/// by a factor of 1.25 or more (the transforms and colour channels of one
/// plane, and an inverted attempt, share its size: never independent).
fn confirmed(live: &[Read], symbology: Symbology, raw: &[u8]) -> bool {
    let mut span: Option<(u32, u32)> = None;
    for read in live
        .iter()
        .filter(|read| read.symbology == symbology && read.key.raw == raw)
    {
        if !read.needs_confirmation {
            return true;
        }
        if let Some((lo, hi)) = read.scales {
            span = Some(span.map_or((lo, hi), |(own_lo, own_hi)| {
                (own_lo.min(lo), own_hi.max(hi))
            }));
        }
    }
    span.is_some_and(|(lo, hi)| u64::from(hi) * 4 >= u64::from(lo) * 5)
}

/// A signalled QR Code read no agreeing candidate parse contains.
fn awaits_location(live: &[Read], read: &Read) -> bool {
    read.key.eci == EciEvidence::Signalled
        && !live.iter().any(|candidate| {
            !candidate.by_engine()
                && candidate.symbology == read.symbology
                && candidate.key.agrees(&read.key)
                && inside(read.location, candidate.location)
        })
}

/// One published content and what its reads measured.
#[derive(Debug, Clone)]
pub(crate) struct Reconciled {
    /// The symbology.
    pub(crate) symbology: Symbology,
    /// Its identity: bytes, the detection's evidence, FNC1 position.
    pub(crate) key: ContentKey,
    /// The one distinct non-empty segment observation of its parsed reads.
    pub(crate) segments: Segments,
    /// Its reads' one engine reading.
    pub(crate) engine_reading: EngineReading,
    /// Engines, in the order of their earliest reads.
    pub(crate) engines: Vec<EngineKind>,
    /// Geometry from its earliest reads that carry it.
    pub(crate) geometry: Geometry,
    /// Its earliest read.
    pub(crate) rank: Rank,
}

/// Whether the centre of a read's corner points lies inside the box of a
/// detection's; reads or detections without corners never fail it.
pub(crate) fn inside(read: Option<[Point; 4]>, detection: Option<[Point; 4]>) -> bool {
    let (Some(read), Some(own)) = (read, detection) else {
        return true;
    };
    let (x, y) = read
        .iter()
        .fold((0.0, 0.0), |(x, y), p| (x + p.x / 4.0, y + p.y / 4.0));
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    for p in own {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
    }
    (min_x..=max_x).contains(&x) && (min_y..=max_y).contains(&y)
}

/// A detection being formed: its account and the reads that joined it.
struct Forming {
    eci: EciEvidence,
    fnc1: Fnc1,
    /// The corner points of the earliest read that formed it.
    place: Option<[Point; 4]>,
    members: Vec<Read>,
}

impl Forming {
    fn new(read: Read) -> Self {
        Self {
            eci: read.key.eci.clone(),
            fnc1: read.key.fnc1,
            place: read.location,
            members: vec![read],
        }
    }

    fn identity(&self) -> Identity {
        identity(&self.eci, self.fnc1)
    }

    fn contains(&self, read: &Read) -> bool {
        inside(read.location, self.place)
    }

    fn agrees(&self, read: &Read) -> bool {
        self.eci.agrees_with(&read.key.eci) && self.fnc1.agrees_with(read.key.fnc1)
    }
}

/// What a detection publishes of its account: the `eci` key (absent, `[]`
/// or the list) and the `fnc1` key.
type Identity = (Option<EciEvidence>, Option<Fnc1>);

fn identity(eci: &EciEvidence, fnc1: Fnc1) -> Identity {
    let eci = matches!(eci, EciEvidence::Located(_) | EciEvidence::Signalled).then(|| eci.clone());
    let fnc1 = matches!(fnc1, Fnc1::First | Fnc1::Second).then_some(fnc1);
    (eci, fnc1)
}

/// Reconcile a read set (the module doc; spec/07 property 6).
pub(crate) fn reconcile(mut reads: Vec<Read>) -> Vec<Reconciled> {
    // a total order: ranks are distinct within a walk; constructed sets
    // that share one fall back to the content
    reads.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then_with(|| format!("{:?}", a.key).cmp(&format!("{:?}", b.key)))
    });
    let admitted: Vec<Read> = reads
        .iter()
        .filter(|read| {
            read.by_engine()
                || reads.iter().any(|engine| {
                    engine.by_engine()
                        && engine.symbology == read.symbology
                        && engine.key.agrees(&read.key)
                        && inside(engine.location, read.location)
                })
        })
        .cloned()
        .collect();
    let mut groups: Vec<Vec<Read>> = Vec::new();
    for read in admitted {
        let group = groups.iter_mut().find(|group| {
            group.first().is_some_and(|first| {
                first.symbology == read.symbology && first.key.raw == read.key.raw
            })
        });
        match group {
            Some(group) => group.push(read),
            None => groups.push(vec![read]),
        }
    }
    let mut detections: Vec<Reconciled> = groups
        .into_iter()
        .flat_map(reconcile_group)
        .filter(|detection| !detection.engines.is_empty())
        .collect();
    detections.sort_by_key(|detection| (!detection.symbology.is_qr_family(), detection.rank));
    detections.truncate(MAX_DETECTIONS);
    detections
}

/// Steps 1-3 within one (symbology, raw) group, its reads in order.
fn reconcile_group(reads: Vec<Read>) -> Vec<Reconciled> {
    let observed = |fnc1: Fnc1| fnc1 != Fnc1::Unobserved;
    let mut formed: Vec<Forming> = Vec::new();
    // step 1: located and no-ECI reads with an observed FNC1 position
    let (first, rest): (Vec<Read>, Vec<Read>) = reads.into_iter().partition(|read| {
        matches!(read.key.eci, EciEvidence::Located(_) | EciEvidence::NoEci)
            && observed(read.key.fnc1)
    });
    for read in first {
        match formed
            .iter_mut()
            .find(|f| f.eci == read.key.eci && f.fnc1 == read.key.fnc1)
        {
            Some(f) => f.members.push(read),
            None => formed.push(Forming::new(read)),
        }
    }
    // step 2: signalled reads with an observed FNC1 position
    let (signalled, rest): (Vec<Read>, Vec<Read>) = rest
        .into_iter()
        .partition(|read| read.key.eci == EciEvidence::Signalled && observed(read.key.fnc1));
    let step_one = formed.len();
    for read in signalled {
        join_signalled(&mut formed, step_one, read);
    }
    // step 3: unobserved evidence with an observed position, then an
    // unobserved position
    let (unobserved, unknown_position): (Vec<Read>, Vec<Read>) =
        rest.into_iter().partition(|read| observed(read.key.fnc1));
    join_or_form(&mut formed, unobserved);
    join_or_form(&mut formed, unknown_position);
    formed.into_iter().map(published).collect()
}

/// Step 2: a signalled read joins the one located step-1 detection of its
/// FNC1 position that contains it; several or none: nothing. Without any
/// such detection, the group's signalled detection for its position, which
/// the first such read opens.
fn join_signalled(formed: &mut Vec<Forming>, step_one: usize, read: Read) {
    let located: Vec<usize> = (0..step_one)
        .filter(|&i| {
            matches!(formed[i].eci, EciEvidence::Located(_)) && formed[i].fnc1 == read.key.fnc1
        })
        .collect();
    if located.is_empty() {
        match formed[step_one..]
            .iter_mut()
            .find(|f| f.fnc1 == read.key.fnc1)
        {
            Some(f) => f.members.push(read),
            None => formed.push(Forming::new(read)),
        }
        return;
    }
    let containing: Vec<usize> = located
        .into_iter()
        .filter(|&i| formed[i].contains(&read))
        .collect();
    if let [only] = containing[..] {
        formed[only].members.push(read);
    }
}

/// Step 3, one kind of read against the detections formed before: a read
/// joins the one agreeing detection that contains it; with several it
/// joins nothing; with none, the reads of that kind form one detection per
/// published identity, unless a detection with that identity already
/// exists — then they join nothing.
fn join_or_form(formed: &mut Vec<Forming>, reads: Vec<Read>) {
    let before = formed.len();
    let mut left: Vec<Read> = Vec::new();
    for read in reads {
        let containing: Vec<usize> = (0..before)
            .filter(|&i| formed[i].agrees(&read) && formed[i].contains(&read))
            .collect();
        match containing[..] {
            [only] => formed[only].members.push(read),
            [] => left.push(read),
            _ => {}
        }
    }
    for read in left {
        let identity = identity(&read.key.eci, read.key.fnc1);
        if formed[..before].iter().any(|f| f.identity() == identity) {
            continue;
        }
        match formed[before..]
            .iter_mut()
            .find(|f| f.identity() == identity)
        {
            Some(f) => f.members.push(read),
            None => formed.push(Forming::new(read)),
        }
    }
}

/// A formed detection as published: its account, the joined segments and
/// engine reading, engines by earliest read, the earliest geometry.
fn published(forming: Forming) -> Reconciled {
    let Forming {
        eci, fnc1, members, ..
    } = forming;
    let mut engines: Vec<(EngineKind, Rank)> = Vec::new();
    for &(engine, rank) in members.iter().flat_map(|m| m.engines.iter()) {
        match engines.iter_mut().find(|(own, _)| *own == engine) {
            Some((_, own)) => *own = (*own).min(rank),
            None => engines.push((engine, rank)),
        }
    }
    engines.sort_by_key(|&(_, rank)| rank);
    // members join in step order, not rank order: each part comes from the
    // member whose read that carries it is the earliest (ranks are distinct
    // within a walk)
    let geometry = Geometry {
        corners: members
            .iter()
            .filter(|m| m.geometry.corners.is_some())
            .min_by_key(|m| m.sources.corners)
            .and_then(|m| m.geometry.corners),
        stream: members
            .iter()
            .filter(|m| m.geometry.stream.is_some())
            .min_by_key(|m| m.sources.stream)
            .and_then(|m| m.geometry.stream.clone()),
        ec: members
            .iter()
            .filter(|m| m.geometry.ec.is_some())
            .min_by_key(|m| m.sources.ec)
            .and_then(|m| m.geometry.ec),
    };
    let rank = members.iter().map(|m| m.rank).min().unwrap_or_default();
    let (symbology, raw) = members
        .first()
        .map_or((Symbology::QrCode, Vec::new()), |m| {
            (m.symbology, m.key.raw.clone())
        });
    Reconciled {
        symbology,
        key: ContentKey { raw, eci, fnc1 },
        segments: joined_segments(&members),
        engine_reading: joined_engine_reading(&members),
        engines: engines.into_iter().map(|(engine, _)| engine).collect(),
        geometry,
        rank,
    }
}

/// The one distinct non-empty segment observation of the parsed reads;
/// several conflict; parsed reads with none: no segment; no parsed read:
/// not observed.
fn joined_segments(members: &[Read]) -> Segments {
    let mut parsed = false;
    let mut seen: Vec<&Vec<DoubleByteSpan>> = Vec::new();
    for member in members.iter().filter(|m| m.parsed) {
        parsed = true;
        if let Segments::Observed(spans) = &member.segments
            && !spans.is_empty()
            && !seen.contains(&spans)
        {
            seen.push(spans);
        }
    }
    match seen[..] {
        [] if parsed => Segments::Observed(Vec::new()),
        [] => Segments::NotObserved,
        [only] => Segments::Observed(only.clone()),
        _ => Segments::Conflicting,
    }
}

/// The one distinct engine reading of the reads; several distinct ones:
/// none (rule 1e does not apply).
fn joined_engine_reading(members: &[Read]) -> EngineReading {
    let mut seen = EngineReading::None;
    for member in members {
        match (seen, member.engine_reading) {
            (_, EngineReading::None) => {}
            (EngineReading::None, reading) => seen = reading,
            (own, reading) if own == reading => {}
            _ => return EngineReading::None,
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use proptest::prelude::*;

    use super::*;
    use crate::engine::content::{self, DoubleByteMode, EciMark};
    use crate::report::Charset;

    use DoubleByteMode::{Hanzi, Kanji};
    use EciEvidence::{NoEci, Signalled, Unobserved};
    use EngineKind::{Rqrr, Rxing};

    const X: Option<EngineKind> = Some(Rxing);
    const Q: Option<EngineKind> = Some(Rqrr);
    /// A candidate parse: no engine.
    const C: Option<EngineKind> = None;
    /// Two places side by side, and none.
    const L: Option<f32> = Some(10.0);
    const R: Option<f32> = Some(210.0);
    const NOWHERE: Option<f32> = None;

    const A1: &[u8] = &[0xA1];
    const E_ACUTE: &[u8] = &[0xC3, 0xA9];
    const YAMADA: &[u8] = &[0x8E, 0x52, 0x93, 0x63];
    const CHUWEN: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4];

    /// A 100 px square at `x`.
    fn square_at(x: f32) -> [Point; 4] {
        [
            Point { x, y: 10.0 },
            Point {
                x: x + 100.0,
                y: 10.0,
            },
            Point {
                x: x + 100.0,
                y: 110.0,
            },
            Point { x, y: 110.0 },
        ]
    }

    fn located(marks: &[(u32, usize)]) -> EciEvidence {
        EciEvidence::Located(
            marks
                .iter()
                .map(|&(value, start)| EciMark { value, start })
                .collect(),
        )
    }

    fn eci4() -> EciEvidence {
        located(&[(4, 0)])
    }

    fn key(raw: &[u8], eci: EciEvidence, fnc1: Fnc1) -> ContentKey {
        ContentKey {
            raw: raw.to_vec(),
            eci,
            fnc1,
        }
    }

    /// The `n`-th read of a set, FNC1 none, made at `at` (a square there);
    /// rqrr reads and candidate parses measure its corners. A candidate
    /// parse is a parse.
    fn read(
        n: u32,
        raw: &[u8],
        eci: EciEvidence,
        engine: Option<EngineKind>,
        at: Option<f32>,
    ) -> Read {
        let rank = Rank {
            attempt: n,
            source: 0,
            index: 0,
        };
        let mut read = Read::new(Symbology::QrCode, key(raw, eci, Fnc1::None), engine, rank)
            .at(at.map(square_at));
        if engine != X {
            read.geometry.corners = at.map(|x| (square_at(x), false));
        }
        if engine.is_none() {
            read.parsed = true;
            read.segments = Segments::Observed(Vec::new());
        }
        read
    }

    /// The read, parsed by the scanner.
    fn parsed(mut read: Read) -> Read {
        read.parsed = true;
        if read.segments == Segments::NotObserved {
            read.segments = Segments::Observed(Vec::new());
        }
        read
    }

    fn fnc1(mut read: Read, fnc1: Fnc1) -> Read {
        read.key.fnc1 = fnc1;
        read
    }

    fn spans(mut read: Read, spans: &[(DoubleByteMode, usize, usize)]) -> Read {
        read.segments = Segments::Observed(
            spans
                .iter()
                .map(|&(mode, start, end)| DoubleByteSpan { mode, start, end })
                .collect(),
        );
        read
    }

    fn engine_reading(mut read: Read, reading: EngineReading) -> Read {
        read.engine_reading = reading;
        read
    }

    /// A detection's reading, as the ladder interprets it.
    fn reading(detection: &Reconciled) -> (String, Charset) {
        let reading = content::read(
            &detection.key.raw,
            &detection.key.eci,
            &detection.segments,
            detection.engine_reading,
            detection.symbology.is_qr_family(),
        );
        (reading.text, reading.charset)
    }

    /// What a publication is, compared across orders.
    fn summary(detections: &[Reconciled]) -> Vec<String> {
        detections
            .iter()
            .map(|d| {
                format!(
                    "{:?} {:?} {:?} {:?} {:?} {:?}",
                    d.key,
                    d.engines,
                    d.rank,
                    d.geometry.corners.map(|(corners, _)| corners[0].x),
                    d.segments,
                    reading(d),
                )
            })
            .collect()
    }

    fn store(reads: impl IntoIterator<Item = Read>, refusals: &[Refusal]) -> ReadStore {
        let mut store = ReadStore::default();
        for refusal in refusals {
            store.refuse(refusal.clone(), 0);
        }
        for read in reads {
            store.collect(read);
        }
        store
    }

    /// Every permutation of `0..n` (Heap's algorithm).
    fn permutations(n: usize) -> Vec<Vec<usize>> {
        fn heap(k: usize, items: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            if k <= 1 {
                out.push(items.clone());
                return;
            }
            heap(k - 1, items, out);
            for i in 0..k - 1 {
                if k.is_multiple_of(2) {
                    items.swap(i, k - 1);
                } else {
                    items.swap(0, k - 1);
                }
                heap(k - 1, items, out);
            }
        }
        let mut out = Vec::new();
        heap(n, &mut (0..n).collect(), &mut out);
        out
    }

    /// Reconcile `reads` collected in every order; the publication must be
    /// one.
    fn every_order(reads: &[Read], refusals: &[Refusal]) -> Vec<Reconciled> {
        let found = store(reads.iter().cloned(), refusals).reconcile();
        let expected = summary(&found);
        for order in permutations(reads.len()) {
            let shuffled = order.iter().map(|&i| reads[i].clone());
            assert_eq!(
                summary(&store(shuffled, refusals).reconcile()),
                expected,
                "order {order:?}"
            );
        }
        found
    }

    fn engines(found: &[Reconciled]) -> Vec<Vec<EngineKind>> {
        found.iter().map(|d| d.engines.clone()).collect()
    }

    #[test]
    fn reconcile_is_order_independent() {
        // rxing and rqrr both located
        let found = every_order(
            &[
                parsed(read(0, A1, eci4(), X, NOWHERE)),
                parsed(read(1, A1, eci4(), Q, L)),
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rxing, Rqrr]]);
        assert_eq!(reading(&found[0]), ("\u{104}".into(), Charset::Eci));
        // a signalled port read joins the located one
        let found = every_order(
            &[
                read(0, A1, Signalled, X, NOWHERE),
                parsed(read(1, A1, eci4(), Q, L)),
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rxing, Rqrr]]);
        assert_eq!(found[0].key.eci, eci4());
        // twins ECI 26 / ECI 4 over the same bytes; a signalled read without
        // a place joins neither, one at the right joins ECI 4
        let twins = [
            parsed(read(0, E_ACUTE, located(&[(26, 0)]), Q, L)),
            parsed(read(1, E_ACUTE, eci4(), Q, R)),
        ];
        for (at, expected) in [
            (NOWHERE, [vec![Rqrr], vec![Rqrr]]),
            (R, [vec![Rqrr], vec![Rqrr, Rxing]]),
        ] {
            let mut reads = twins.to_vec();
            reads.push(read(2, E_ACUTE, Signalled, X, at));
            let found = every_order(&reads, &[]);
            assert_eq!(engines(&found), expected, "{at:?}");
            assert_eq!(reading(&found[0]), ("é".into(), Charset::Utf8));
            assert_eq!(reading(&found[1]), ("ĂŠ".into(), Charset::Eci));
        }
        // FNC1 first and second are two contents
        let found = every_order(
            &[
                fnc1(read(0, b"37abc", NoEci, X, NOWHERE), Fnc1::First),
                fnc1(read(1, b"37abc", NoEci, X, NOWHERE), Fnc1::Second),
            ],
            &[],
        );
        assert_eq!(found.len(), 2);
        // an unobserved read joins the one it lies in, none without a place
        let pair = [
            parsed(read(0, A1, NoEci, Q, L)),
            parsed(read(1, A1, eci4(), Q, R)),
        ];
        for (at, expected) in [
            (R, [vec![Rqrr], vec![Rqrr, Rxing]]),
            (NOWHERE, [vec![Rqrr], vec![Rqrr]]),
        ] {
            let mut reads = pair.to_vec();
            reads.push(read(2, A1, Unobserved, X, at));
            assert_eq!(engines(&every_order(&reads, &[])), expected, "{at:?}");
        }
        // FNC1 first + ECI 26, the port read located by the re-read
        let gs1: &[u8] = b"0109506000134352";
        let eci26 = located(&[(26, 0)]);
        let found = every_order(
            &[
                fnc1(parsed(read(0, gs1, eci26.clone(), X, L)), Fnc1::First),
                fnc1(read(1, gs1, eci26, C, L), Fnc1::First),
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rxing]]);
        assert_eq!(found[0].key.fnc1, Fnc1::First);
        assert!(found[0].geometry.corners.is_some(), "the candidate's grid");
        // signalled beside no-ECI over the same bytes
        let found = every_order(
            &[
                read(0, E_ACUTE, Signalled, X, NOWHERE),
                read(1, E_ACUTE, NoEci, X, NOWHERE),
            ],
            &[],
        );
        assert_eq!(
            found.iter().map(reading).collect::<Vec<_>>(),
            [
                ("\u{FFFD}\u{FFFD}".to_owned(), Charset::Unknown),
                ("é".to_owned(), Charset::Utf8)
            ]
        );
    }

    #[test]
    fn double_byte_contents_reconcile_in_every_order() {
        // Kanji 山田, rxing (its own reading) + rqrr (segments)
        let found = every_order(
            &[
                engine_reading(read(0, YAMADA, NoEci, X, NOWHERE), EngineReading::ShiftJis),
                spans(parsed(read(1, YAMADA, NoEci, Q, L)), &[(Kanji, 0, 4)]),
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rxing, Rqrr]]);
        assert_eq!(reading(&found[0]), ("山田".into(), Charset::ShiftJis));
        // Hanzi 中文, rxing + a candidate parse (no engine)
        let found = every_order(
            &[
                read(0, CHUWEN, NoEci, X, L),
                spans(read(1, CHUWEN, NoEci, C, L), &[(Hanzi, 0, 4)]),
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rxing]]);
        assert_eq!(reading(&found[0]), ("中文".into(), Charset::Gb18030));
        // Hanzi twice, one unobserved
        let found = every_order(
            &[
                engine_reading(read(0, CHUWEN, NoEci, X, NOWHERE), EngineReading::Gb18030),
                read(1, CHUWEN, Unobserved, Q, NOWHERE),
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rxing, Rqrr]]);
        assert_eq!(reading(&found[0]), ("中文".into(), Charset::Gb18030));
        // Kanji-mode and byte-mode twins
        let found = every_order(
            &[
                spans(parsed(read(0, YAMADA, NoEci, Q, L)), &[(Kanji, 0, 4)]),
                parsed(read(1, YAMADA, NoEci, Q, R)),
            ],
            &[],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(reading(&found[0]), ("山田".into(), Charset::ShiftJis));
        // Kanji against Hanzi over one stream: conflicting segments
        let both: &[u8] = &[0xB0, 0xA1, 0xB0, 0xA1];
        let found = every_order(
            &[
                spans(parsed(read(0, both, NoEci, Q, L)), &[(Kanji, 0, 4)]),
                spans(parsed(read(1, both, NoEci, X, R)), &[(Hanzi, 0, 4)]),
            ],
            &[],
        );
        assert_eq!(found[0].segments, Segments::Conflicting);
        assert_eq!(reading(&found[0]), ("\u{FFFD}".repeat(4), Charset::Unknown));
    }

    #[test]
    fn reads_beside_candidates_and_refusals_reconcile_in_every_order() {
        // an unobserved read without a place between two candidates
        let found = every_order(
            &[
                read(0, A1, Unobserved, X, NOWHERE),
                read(1, A1, NoEci, C, L),
                read(2, A1, eci4(), C, R),
            ],
            &[],
        );
        assert!(found.is_empty(), "{:?}", summary(&found));
        // rxing-only signalled twins are one content
        let found = every_order(
            &[
                read(0, E_ACUTE, Signalled, X, L),
                read(1, E_ACUTE, Signalled, X, R),
            ],
            &[],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            reading(&found[0]),
            ("\u{FFFD}\u{FFFD}".into(), Charset::Unknown)
        );
        // with a refusal (the place rules have their own tests below)
        let refusal = Refusal {
            port: Some(key(b"abc", Signalled, Fnc1::None)),
            rqrr: None,
            place: None,
        };
        let found = every_order(
            &[
                parsed(read(0, b"abc", located(&[(3, 0)]), Q, L)),
                read(1, b"abc", Signalled, X, L),
                read(2, b"abc", Signalled, X, R),
            ],
            &[refusal],
        );
        assert_eq!(engines(&found), [vec![Rqrr, Rxing]], "protected, joined");
    }

    #[test]
    fn a_read_outside_its_only_candidate_never_lends_geometry() {
        // a located anchor on the left and an unobserved read of another
        // symbol on the right: two contents, each its own geometry
        let found = every_order(
            &[
                parsed(read(0, A1, eci4(), Q, L)),
                read(1, A1, Unobserved, Q, R),
            ],
            &[],
        );
        assert_eq!(found.len(), 2);
        let corners: Vec<Option<f32>> = found
            .iter()
            .map(|d| d.geometry.corners.map(|(c, _)| c[0].x))
            .collect();
        assert_eq!(corners, [L, R]);
        assert_eq!(reading(&found[1]), ("\u{FFFD}".into(), Charset::Unknown));
        // a signalled read beside a single located anchor joins none
        let found = every_order(
            &[
                parsed(read(0, A1, eci4(), Q, L)),
                read(1, A1, Signalled, X, R),
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rqrr]]);
    }

    #[test]
    fn reads_without_place_join_none_and_order_never_matters() {
        // U (no place), N and S of [A1]: two detections; U agrees with both
        // and lies in both — it joins neither (their earliest reads stay N's
        // and S's)
        let found = every_order(
            &[
                read(0, A1, Unobserved, Q, NOWHERE),
                read(1, A1, NoEci, Q, L),
                read(2, A1, Signalled, X, R),
            ],
            &[],
        );
        let ranks: Vec<u32> = found.iter().map(|d| d.rank.attempt).collect();
        assert_eq!(ranks, [1, 2]);
        assert_eq!(found[0].key.eci, NoEci);
        assert_eq!(found[1].key.eci, Signalled);
    }

    #[test]
    fn a_candidate_joins_only_where_it_contains_an_engine_read() {
        // the only matching port read lies elsewhere: the candidate is not
        // admitted, the port read stands alone without geometry
        let found = every_order(
            &[
                fnc1(read(0, b"10ABC", NoEci, C, L), Fnc1::First),
                fnc1(read(1, b"10ABC", NoEci, X, R), Fnc1::First),
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rxing]]);
        assert!(found[0].geometry.corners.is_none());
    }

    #[test]
    fn a_read_two_candidates_contain_joins_neither() {
        // both candidates admitted, the read joins neither, neither is
        // published
        let found = every_order(
            &[
                read(0, A1, eci4(), C, L),
                read(1, A1, located(&[(7, 0)]), C, L),
                read(2, A1, Signalled, X, L),
            ],
            &[],
        );
        assert!(found.is_empty(), "{:?}", summary(&found));
    }

    #[test]
    fn reconciliation_follows_the_evidence_lattice() {
        // (first account, second account, detections, account kept first)
        let cases = [
            (Unobserved, NoEci, 1, NoEci),
            (NoEci, Unobserved, 1, NoEci),
            (Unobserved, eci4(), 1, eci4()),
            (Signalled, Signalled, 1, Signalled),
            (Signalled, eci4(), 1, eci4()),
            (eci4(), Signalled, 1, eci4()),
            (eci4(), eci4(), 1, eci4()),
            (NoEci, Signalled, 2, NoEci),
            (NoEci, eci4(), 2, NoEci),
            (eci4(), located(&[(4, 1)]), 2, eci4()),
            (eci4(), located(&[(3, 0)]), 2, eci4()),
        ];
        for (first, second, count, kept) in cases {
            let found = every_order(
                &[
                    read(0, A1, first.clone(), X, NOWHERE),
                    read(1, A1, second.clone(), Q, NOWHERE),
                ],
                &[],
            );
            assert_eq!(found.len(), count, "{first:?} then {second:?}");
            assert_eq!(found[0].key.eci, kept, "{first:?} then {second:?}");
        }
    }

    #[test]
    fn fnc1_first_and_second_never_reconcile() {
        // the same bytes under GS1 (first) and under an AIM application
        // (second) are two contents, a plain read a third; an unknown
        // position agrees with all three and joins none
        let found = every_order(
            &[
                fnc1(read(0, b"37abc", NoEci, Q, NOWHERE), Fnc1::Unobserved),
                fnc1(read(1, b"37abc", NoEci, X, NOWHERE), Fnc1::First),
                fnc1(
                    read(2, b"37abc", NoEci, Some(EngineKind::Rescue), NOWHERE),
                    Fnc1::Second,
                ),
                read(3, b"37abc", NoEci, Q, NOWHERE),
            ],
            &[],
        );
        let positions: Vec<Fnc1> = found.iter().map(|d| d.key.fnc1).collect();
        assert_eq!(positions, [Fnc1::First, Fnc1::Second, Fnc1::None]);
        assert!(found.iter().all(|d| d.engines.len() == 1));
    }

    #[test]
    fn a_read_with_corners_joins_the_admitting_detection_it_overlaps() {
        // two symbols of [A1] side by side — unmarked on the left, ECI 4 on
        // the right — and a read both admit: where it lies decides; a read
        // WITHOUT a place both admit joins none
        let base = [
            parsed(read(0, A1, NoEci, Q, L)),
            parsed(read(1, A1, eci4(), Q, R)),
            read(2, A1, Unobserved, Some(EngineKind::Rescue), Some(212.0)),
        ];
        let found = every_order(&base, &[]);
        assert_eq!(
            engines(&found),
            [vec![Rqrr], vec![Rqrr, EngineKind::Rescue]]
        );
        // (its FNC1 position unobserved too, so it never folds into the
        // rescue read)
        let mut reads = base.to_vec();
        reads.push(fnc1(read(3, A1, Unobserved, X, NOWHERE), Fnc1::Unobserved));
        assert_eq!(
            engines(&every_order(&reads, &[])),
            [vec![Rqrr], vec![Rqrr, EngineKind::Rescue]]
        );
    }

    #[test]
    fn identical_reads_fold_with_the_first_geometry() {
        // engines united; the first corners keep their photometry, whatever
        // the order the reads arrive in
        let mut later = read(1, b"same", NoEci, Q, R);
        later.geometry.corners = Some((square_at(210.0), true));
        let found = every_order(
            &[
                read(0, b"same", NoEci, X, NOWHERE),
                read(2, b"same", NoEci, Q, L),
                later,
            ],
            &[],
        );
        assert_eq!(engines(&found), [vec![Rxing, Rqrr]]);
        assert_eq!(
            found[0]
                .geometry
                .corners
                .map(|(c, inverted)| (c[0].x, inverted)),
            Some((210.0, true)),
            "the earliest read that carries corners"
        );
    }

    #[test]
    fn reconcile_bounds() {
        // 65 distinct reads — 64 reconciled (the 65th counted), at most 16
        // published, the QR family first
        let mut store = ReadStore::default();
        for i in 0..65u32 {
            let mut read = read(i, format!("symbol-{i}").as_bytes(), NoEci, X, NOWHERE);
            if i % 2 == 0 {
                read.symbology = Symbology::DataMatrix;
                read.key.eci = Unobserved;
                read.key.fnc1 = Fnc1::Unobserved;
            }
            store.collect(read);
        }
        assert_eq!((store.reads.len(), store.dropped), (MAX_DISTINCT_READS, 1));
        let found = store.reconcile();
        assert_eq!(found.len(), MAX_DETECTIONS);
        assert!(found.iter().all(|d| d.symbology == Symbology::QrCode));
        assert_eq!(found[0].key.raw, b"symbol-1");
        // a known content still folds in past the bound
        store.collect(read(70, b"symbol-1", NoEci, Q, NOWHERE));
        assert_eq!(store.reconcile()[0].engines, [Rxing, Rqrr]);
    }

    fn port(raw: &[u8], place: Option<f32>) -> Refusal {
        Refusal {
            port: Some(key(raw, Signalled, Fnc1::None)),
            rqrr: None,
            place: place.map(square_at),
        }
    }

    fn published(store: &ReadStore) -> Vec<(Vec<u8>, Vec<EngineKind>)> {
        store
            .reconcile()
            .into_iter()
            .map(|d| (d.key.raw, d.engines))
            .collect()
    }

    #[test]
    fn a_refusal_withholds_the_reads_its_names_cover_and_nothing_else() {
        // the port key (abc, signalled, none) withholds unparsed QR Code
        // reads (abc, signalled) and (abc, unobserved) — retroactively
        let mut store = store(
            [
                read(0, b"abc", Signalled, X, NOWHERE),
                read(1, b"abc", Unobserved, Q, NOWHERE),
            ],
            &[],
        );
        assert_eq!(published(&store).len(), 1);
        store.refuse(port(b"abc", None), 2);
        assert!(
            published(&store).is_empty(),
            "a later proof marks earlier reads"
        );
        // never a no-ECI read, a GS1 one, a parsed read or another symbology
        let mut micro = read(4, b"abc", Signalled, X, NOWHERE);
        micro.symbology = Symbology::MicroQrCode;
        for other in [
            read(3, b"abc", NoEci, X, NOWHERE),
            fnc1(read(3, b"abc", Signalled, X, NOWHERE), Fnc1::First),
            parsed(read(3, b"abc", eci4(), X, NOWHERE)),
            micro,
        ] {
            let store = store_with(other.clone(), port(b"abc", None));
            assert_eq!(published(&store).len(), 1, "{other:?}");
        }
        // rqrr's bytes withhold rqrr's part of an unparsed read only
        let refusal = Refusal {
            port: None,
            rqrr: Some(b"xyz".to_vec()),
            place: None,
        };
        let store = store_with(read(0, b"xyz", Unobserved, Q, NOWHERE), refusal.clone());
        assert!(published(&store).is_empty());
        let mut both = read(0, b"xyz", Unobserved, Q, NOWHERE);
        both.engines.push((
            Rxing,
            Rank {
                attempt: 1,
                ..Rank::default()
            },
        ));
        assert_eq!(
            published(&store_with(both, refusal)),
            [(b"xyz".to_vec(), vec![Rxing])]
        );
    }

    fn store_with(read: Read, refusal: Refusal) -> ReadStore {
        let mut store = ReadStore::default();
        store.collect(read);
        store.refuse(refusal, 0);
        store
    }

    #[test]
    fn a_refusal_withholds_only_where_the_refused_symbol_is() {
        // a well-formed symbol beside a refused one, their port reads alike —
        // the refusal withholds the read that lies inside the refused grid,
        // never the other
        let store = store_with(read(0, b"abc", Signalled, X, L), port(b"abc", R));
        assert_eq!(published(&store).len(), 1, "elsewhere: it stands");
        let store = store_with(read(0, b"abc", Signalled, X, R), port(b"abc", R));
        assert!(published(&store).is_empty(), "inside: withheld");
        // a read made at both places is evidence of a symbol elsewhere
        let mut twice = read(0, b"abc", Signalled, X, R);
        twice.fold(read(1, b"abc", Signalled, X, L));
        assert_eq!(published(&store_with(twice, port(b"abc", R))).len(), 1);
        // a read without a place never fails the place test
        let store = store_with(read(0, b"abc", Signalled, X, NOWHERE), port(b"abc", R));
        assert!(published(&store).is_empty());
    }

    #[test]
    fn a_parsed_read_of_the_same_attempt_protects_an_earlier_read() {
        // a proof and a parsed read admitting an earlier observation in one
        // attempt — the observation is kept, with its engines and first-seen
        // order
        let mut store = ReadStore::default();
        store.collect(read(0, b"abc", Signalled, X, NOWHERE));
        store.collect(read(1, b"zzz", NoEci, X, NOWHERE));
        store.refuse(port(b"abc", None), 2);
        store.collect(parsed(read(2, b"abc", eci4(), Q, L)));
        assert_eq!(
            published(&store),
            [
                (b"abc".to_vec(), vec![Rxing, Rqrr]),
                (b"zzz".to_vec(), vec![Rxing])
            ]
        );
        // so does the other walk's parsed content (the alpha seed)
        let mut seeded = ReadStore::seeded(Seed {
            refusals: vec![port(b"abc", None)],
            protection: vec![(Symbology::QrCode, key(b"abc", eci4(), Fnc1::None))],
            ..Seed::default()
        });
        seeded.collect(read(0, b"abc", Signalled, X, NOWHERE));
        assert_eq!(published(&seeded).len(), 1);
    }

    #[test]
    fn a_refusal_past_capacity_withholds_up_to_its_attempt() {
        // 16 recorded refusals; a 17th, found in attempt 3, withholds what
        // was read up to attempt 3 — a later attempt's read stands
        let mut store = ReadStore::default();
        for i in 0..MAX_REFUSALS {
            store.refuse(port(&[u8::try_from(i).unwrap()], None), 0);
        }
        store.collect(read(3, b"zzz", Signalled, X, NOWHERE));
        store.refuse(port(b"zzz", None), 3);
        assert_eq!(store.refusals.len(), MAX_REFUSALS, "the capacity holds");
        assert!(published(&store).is_empty());
        store.collect(read(3, b"zzz", Signalled, X, NOWHERE));
        assert!(published(&store).is_empty(), "the same attempt's read too");
        store.collect(read(4, b"zzz", Signalled, X, NOWHERE));
        assert_eq!(published(&store).len(), 1, "then it lapses");
        // the opposite-background walk receives it
        assert_eq!(store.seed(0).refusals.len(), MAX_REFUSALS + 1);
        let mut walk2 = ReadStore::seeded(store.seed(0));
        walk2.collect(read(9, b"zzz", Signalled, X, NOWHERE));
        assert!(published(&walk2).is_empty());
    }

    /// A port refusal of `raw` at a 100 px square at `x`.
    fn port_at(raw: &[u8], x: f32) -> Refusal {
        port(raw, Some(x))
    }

    /// The publication as a set: what stands, never in which order.
    fn published_set(store: &ReadStore) -> Vec<(Vec<u8>, Vec<EngineKind>)> {
        let mut found = published(store);
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    #[test]
    fn a_refused_stream_is_one_record_wherever_it_is_refused() {
        // a record is keyed by the stream's names, never by where it was
        // refused. One name refused at 17 jittered places (one symbol
        // rescaled at every attempt) is one record, so a second name refused
        // after it is a full-strength record: its read stays withheld after
        // the attempt that refused it
        let mut store = ReadStore::default();
        for i in 0..=16u16 {
            store.refuse(port_at(b"aaa", 10.0 + f32::from(i) * 0.25), u32::from(i));
        }
        store.refuse(port(b"bbb", R), 17);
        store.collect(read(17, b"bbb", Signalled, X, R));
        store.collect(read(18, b"bbb", Signalled, X, R));
        assert!(published(&store).is_empty(), "{:?}", published(&store));
        assert_eq!(store.refusals.len(), 2, "one record per name");
    }

    #[test]
    fn a_refusal_found_again_past_capacity_withholds_up_to_its_latest_attempt() {
        // 16 refused streams fill the capacity; a 17th found again at the
        // same place in a later attempt withholds up to that attempt — then
        // lapses where it is not found again
        let mut store = ReadStore::default();
        for i in 0..MAX_REFUSALS {
            store.refuse(port(&[u8::try_from(i).unwrap()], None), 0);
        }
        store.refuse(port(b"zzz", R), 17);
        store.refuse(port(b"zzz", R), 18);
        store.collect(read(18, b"zzz", Signalled, X, R));
        assert!(published(&store).is_empty(), "found again: withheld");
        store.collect(read(19, b"zzz", Signalled, X, R));
        assert_eq!(published(&store).len(), 1, "then it lapses");
    }

    #[test]
    fn refusals_and_reads_withhold_alike_in_every_arrival_order() {
        // within capacity, what a walk withholds depends on its refusals and
        // reads, never on their order. One symbol refused at 15 jittered
        // places first; then a small set in every order, each event at the
        // attempt it arrives in: a 16th place of that symbol, a second
        // stream, its read, a read of the first content elsewhere (another
        // symbol: it stands) and an unrelated read
        type Event = fn(&mut ReadStore, u32);
        let events: [Event; 5] = [
            |store, attempt| store.refuse(port_at(b"aaa", 14.0), attempt),
            |store, attempt| store.refuse(port(b"bbb", R), attempt),
            |store, attempt| store.collect(read(attempt, b"bbb", Signalled, X, R)),
            |store, attempt| store.collect(read(attempt, b"aaa", Signalled, X, Some(410.0))),
            |store, attempt| store.collect(read(attempt, b"zzz", NoEci, X, NOWHERE)),
        ];
        for order in permutations(events.len()) {
            let mut store = ReadStore::default();
            for i in 0..15u16 {
                store.refuse(port_at(b"aaa", 10.0 + f32::from(i) * 0.25), u32::from(i));
            }
            for (k, &event) in order.iter().enumerate() {
                events[event](&mut store, 15 + u32::try_from(k).unwrap());
            }
            assert_eq!(
                published_set(&store),
                [
                    (b"aaa".to_vec(), vec![Rxing]),
                    (b"zzz".to_vec(), vec![Rxing])
                ],
                "order {order:?}"
            );
        }
    }

    #[test]
    fn the_refusal_bookkeeping_is_bounded_and_fails_closed() {
        // past capacity at most as many records again are kept, never one
        // per refused grid of every attempt; past that the walk fails
        // closed — every unparsed QR Code read collected up to that attempt
        // is withheld, a parsed one never
        let mut store = ReadStore::default();
        store.collect(read(500, b"plain", NoEci, X, NOWHERE));
        store.collect(parsed(read(500, b"parsed", eci4(), Q, L)));
        for i in 0..1000u32 {
            store.refuse(port(format!("s{i}").as_bytes(), None), i);
        }
        assert!(
            store.overflow.len() <= MAX_REFUSALS,
            "{}",
            store.overflow.len()
        );
        assert_eq!(published_set(&store), [(b"parsed".to_vec(), vec![Rqrr])]);
        store.collect(read(1000, b"later", NoEci, X, NOWHERE));
        assert_eq!(published(&store).len(), 2, "a later read stands");
        // the opposite-background walk starts from a bounded seed and fails
        // closed throughout
        let seed = store.seed(0);
        assert!(
            seed.refusals.len() <= 2 * MAX_REFUSALS,
            "{}",
            seed.refusals.len()
        );
        let mut walk2 = ReadStore::seeded(seed);
        walk2.collect(read(3, b"other", NoEci, X, NOWHERE));
        assert!(published(&walk2).is_empty());
    }

    #[test]
    fn a_read_made_at_nine_scales_of_one_symbol_keeps_a_place_for_its_twin() {
        // places are told apart by containment, never by exact equality:
        // one symbol read at nine attempt scales is one place, so a twin
        // read elsewhere is recorded too and a refusal of the first symbol
        // never withholds the folded read — whichever arrives last
        let scales: Vec<Read> = (1..=9u16)
            .map(|i| {
                read(
                    u32::from(i),
                    b"abc",
                    Signalled,
                    X,
                    Some(10.0 + f32::from(i) * 0.1),
                )
            })
            .collect();
        let twin = read(10, b"abc", Signalled, X, R);
        for twin_first in [true, false] {
            let mut reads = scales.clone();
            if twin_first {
                reads.insert(0, twin.clone());
            } else {
                reads.push(twin.clone());
            }
            let store = store(reads, &[port(b"abc", L)]);
            assert_eq!(published(&store).len(), 1, "twin first: {twin_first}");
            assert_eq!(store.reads[0].places.len(), 2, "one place per symbol");
        }
    }

    #[test]
    fn geometry_comes_from_the_detections_earliest_read_that_carries_it() {
        // spec/07: geometry comes from the detection's earliest read that
        // carries it. An unobserved rqrr read (attempt 0, corners) joins the
        // detection a later parsed read forms (attempt 1) in step 3: the
        // corners and their photometry are the earlier read's, in any order
        let mut early = read(0, A1, Unobserved, Q, L);
        early.geometry.corners = Some((square_at(10.0), true));
        let mut later = parsed(read(1, A1, NoEci, Q, Some(12.0)));
        later.geometry.corners = Some((square_at(12.0), false));
        let found = every_order(&[early, later], &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0]
                .geometry
                .corners
                .map(|(corners, inverted)| (corners[0].x, inverted)),
            Some((10.0, true))
        );
    }

    #[test]
    fn the_walk_goes_on_while_only_signalled_qr_reads_were_found() {
        // continuation over the reads that stand
        let settled = |reads: Vec<Read>, refusals: &[Refusal]| store(reads, refusals).settled();
        assert!(!settled(Vec::new(), &[]), "nothing found");
        assert!(!settled(vec![read(0, b"abc", Signalled, X, NOWHERE)], &[]));
        let mut ean = read(1, b"4006381333931", Unobserved, X, NOWHERE);
        ean.symbology = Symbology::Ean13;
        assert!(
            !settled(
                vec![read(0, b"abc", Signalled, X, NOWHERE), ean.clone()],
                &[]
            ),
            "other symbologies neither stop nor prolong it"
        );
        assert!(settled(vec![ean], &[]));
        for account in [NoEci, Unobserved, located(&[(26, 0)])] {
            assert!(
                settled(
                    vec![
                        read(0, b"abc", Signalled, X, NOWHERE),
                        read(1, b"xyz", account.clone(), X, NOWHERE),
                    ],
                    &[]
                ),
                "{account:?}"
            );
        }
        for symbology in [Symbology::MicroQrCode, Symbology::RectangularMicroQrCode] {
            let mut other = read(0, b"abc", Signalled, X, NOWHERE);
            other.symbology = symbology;
            assert!(settled(vec![other], &[]), "{symbology:?}");
        }
        // withheld reads do not count
        assert!(!settled(
            vec![
                read(0, b"abc", Signalled, X, NOWHERE),
                read(1, b"zzz", Signalled, X, NOWHERE),
                read(2, b"zzz", Unobserved, Q, NOWHERE),
            ],
            &[port(b"zzz", None)]
        ));
        // a signalled read a candidate parse contains with an agreeing
        // account counts as located
        assert!(settled(
            vec![
                read(0, b"abc", Signalled, X, L),
                read(1, b"abc", located(&[(26, 0)]), C, L),
            ],
            &[]
        ));
        assert!(!settled(
            vec![
                read(0, b"abc", Signalled, X, L),
                read(1, b"abc", located(&[(26, 0)]), C, R),
            ],
            &[]
        ));
    }

    fn arbitrary_read() -> impl Strategy<Value = (u8, u8, u8, u8, u8)> {
        (0u8..2, 0u8..5, 0u8..3, 0u8..3, 0u8..3)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        /// Order independence over arbitrary sets of up to 64 reads.
        #[test]
        fn reconcile_is_order_independent_over_arbitrary_sets(
            shapes in proptest::collection::vec(arbitrary_read(), 1..64)
                .prop_flat_map(|shapes| {
                    let n = shapes.len();
                    (Just(shapes), Just((0..n).collect::<Vec<usize>>()).prop_shuffle())
                }),
        ) {
            let (shapes, order) = shapes;
            let reads: Vec<Read> = shapes
                .iter()
                .enumerate()
                .map(|(n, &(raw, eci, fnc1_code, engine, at))| {
                    let raw: &[u8] = if raw == 0 { A1 } else { b"abc" };
                    let eci = match eci {
                        0 => Unobserved,
                        1 => NoEci,
                        2 => Signalled,
                        3 => eci4(),
                        _ => located(&[(7, 0)]),
                    };
                    let engine = [X, Q, C][usize::from(engine)];
                    let at = [NOWHERE, L, R][usize::from(at)];
                    let position = [Fnc1::None, Fnc1::First, Fnc1::Unobserved][usize::from(fnc1_code)];
                    let mut read = fnc1(read(u32::try_from(n).unwrap(), raw, eci, engine, at), position);
                    if engine == Q && matches!(read.key.eci, EciEvidence::Located(_) | NoEci) {
                        read = parsed(read);
                    }
                    read
                })
                .collect();
            let expected = summary(&store(reads.iter().cloned(), &[]).reconcile());
            let shuffled = order.iter().map(|&i| reads[i].clone());
            prop_assert_eq!(summary(&store(shuffled, &[]).reconcile()), expected);
        }
    }

    /// The read, with a known correction margin of its own word.
    fn margin(mut read: Read, m: i32) -> Read {
        read.margin = Margin::Known(m);
        read
    }

    #[test]
    fn a_content_whose_every_known_margin_is_negative_is_refused() {
        // 07-pipeline § Acceptance: errors alone took every measured word of
        // the content past the Annex B reserve
        let rqrr = |m| margin(parsed(read(1, b"abc", NoEci, Q, L)), m);
        let rxing = read(0, b"abc", NoEci, X, L);
        // alone: nothing published, nothing settled, S5 not stopped
        let alone = store([rqrr(-1)], &[]);
        assert!(alone.reconcile().is_empty() && !alone.settled() && !alone.found());
        // a read of unknown margin inherits the refusal
        assert!(store([rqrr(-1), rxing.clone()], &[]).reconcile().is_empty());
        // one read keeping a margin confirms the content: one detection, the
        // refused word's engine among its engines
        let found = every_order(&[rqrr(-1), margin(rxing.clone(), 0)], &[]);
        assert_eq!(engines(&found), [vec![Rxing, Rqrr]]);
        // two negative margins never confirm each other
        assert!(every_order(&[rqrr(-1), margin(rxing.clone(), -2)], &[]).is_empty());
        // no known margin: published as before
        assert_eq!(
            engines(&store([rxing.clone()], &[]).reconcile()),
            [vec![Rxing]]
        );
        // another content is not touched: other bytes, or an account the
        // refused one does not agree with
        let other = read(2, b"xyz", NoEci, X, R);
        let declared = read(3, b"abc", located(&[(26, 0)]), X, R);
        let found = every_order(&[rqrr(-1), other, declared], &[]);
        assert_eq!(
            found.iter().map(|d| d.key.raw.clone()).collect::<Vec<_>>(),
            [b"xyz".to_vec(), b"abc".to_vec()]
        );
        // folded reads keep their best margin
        let mut folded = store([rqrr(-1)], &[]);
        folded.collect(margin(parsed(read(4, b"abc", NoEci, Q, L)), 2));
        assert_eq!(engines(&folded.reconcile()), [vec![Rqrr]]);
    }

    /// A UPC-E read of `raw` made on a plane whose longest side is `side`.
    fn upce(n: u32, raw: &[u8], side: u32) -> Read {
        let mut read = fnc1(read(n, raw, Unobserved, X, NOWHERE), Fnc1::Unobserved);
        read.symbology = Symbology::UpcE;
        read.needs_confirmation = true;
        read.scales = Some((side, side));
        read
    }

    #[test]
    fn a_read_that_needs_confirmation_stands_on_two_independent_scales() {
        let raw = b"04917052";
        // one plane: provisional — nothing published or settled, S5 runs
        let one = store([upce(0, raw, 512)], &[]);
        assert!(one.reconcile().is_empty() && !one.settled() && !one.found());
        assert!(!one.confirmed(Symbology::UpcE, raw));
        // planes of equal dimensions never confirm each other
        assert!(
            store([upce(0, raw, 552), upce(1, raw, 552)], &[])
                .reconcile()
                .is_empty()
        );
        // longest sides 1.25 apart do; just under does not
        let two = store([upce(0, raw, 512), upce(1, raw, 640)], &[]);
        assert_eq!(two.reconcile().len(), 1);
        assert!(two.settled() && two.found() && two.confirmed(Symbology::UpcE, raw));
        assert!(
            store([upce(0, raw, 512), upce(1, raw, 639)], &[])
                .reconcile()
                .is_empty()
        );
        // other bytes of the symbology confirm nothing
        assert!(
            store([upce(0, raw, 512), upce(1, b"15658982", 1024)], &[])
                .reconcile()
                .is_empty()
        );
        // an EAN-13 read alone stands and settles
        let mut ean = read(0, b"9506000134352", Unobserved, X, NOWHERE);
        ean.symbology = Symbology::Ean13;
        ean.scales = Some((512, 512));
        assert!(store([ean], &[]).settled());
        // the decision is one of the read set, in every order
        let reads = [upce(0, raw, 512), upce(1, raw, 552), upce(2, raw, 1024)];
        assert_eq!(every_order(&reads, &[]).len(), 1);
    }

    #[test]
    fn a_fold_unites_the_parse_keeps_eight_places_and_waits_only_if_both_wait() {
        // one read parsed and one not, identical otherwise: the stream was
        // parsed, whichever folds in
        let unparsed = read(0, A1, NoEci, X, L);
        let mut parsed_too = read(1, A1, NoEci, X, L);
        parsed_too.parsed = true;
        let mut folded = unparsed.clone();
        folded.fold(parsed_too.clone());
        assert!(folded.parsed);
        let mut folded = parsed_too;
        folded.fold(unparsed);
        assert!(folded.parsed);
        // a read that needs no confirmation clears the wait; two that wait,
        // wait
        let mut waiting = upce(0, b"04917052", 512);
        let mut settled = upce(1, b"04917052", 512);
        settled.needs_confirmation = false;
        waiting.fold(settled);
        assert!(!waiting.needs_confirmation);
        let mut waiting = upce(0, b"04917052", 512);
        waiting.fold(upce(1, b"04917052", 640));
        assert!(waiting.needs_confirmation);
        // nine symbols, 200 px apart: the first eight places are kept
        let mut many = read(0, A1, NoEci, X, Some(0.0));
        for (n, x) in (1u32..).zip([200.0, 400.0, 600.0, 800.0, 1000.0, 1200.0, 1400.0, 1600.0]) {
            many.fold(read(n, A1, NoEci, X, Some(x)));
        }
        assert_eq!(many.places.len(), MAX_PLACES);
        assert_eq!(many.places.last().map(|place| place[0].x), Some(1400.0));
    }

    #[test]
    fn a_refused_stream_remembers_eight_symbols_then_withholds_anywhere() {
        let mut refused = Refused::new(port(b"abc", Some(0.0)), 0);
        // a second symbol: both remembered, a read elsewhere is not held
        refused.again(Some(square_at(200.0)), 1);
        assert_eq!(refused.places.as_ref().map(Vec::len), Some(2));
        assert!(refused.holds(square_at(200.0)) && !refused.holds(square_at(400.0)));
        for (attempt, x) in (2u32..).zip([400.0, 600.0, 800.0, 1000.0, 1200.0, 1400.0]) {
            refused.again(Some(square_at(x)), attempt);
        }
        assert_eq!(refused.places.as_ref().map(Vec::len), Some(MAX_PLACES));
        assert!(!refused.holds(square_at(1600.0)));
        // a ninth symbol: refused at more than it remembers, anywhere
        refused.again(Some(square_at(1600.0)), 8);
        assert!(refused.places.is_none());
        assert!(refused.holds(square_at(5000.0)));
        assert_eq!(refused.attempt, 8);
    }

    #[test]
    fn rqrr_s_part_is_withheld_only_by_rqrr_s_own_bytes() {
        // a refusal naming another content where an rqrr read lies: no part
        // of that read is withheld, whichever name the refusal carries
        for refusal in [
            port(b"xyz", L),
            Refusal {
                port: None,
                rqrr: Some(b"xyz".to_vec()),
                place: L.map(square_at),
            },
        ] {
            let store = store_with(read(0, A1, Unobserved, Q, L), refusal.clone());
            assert_eq!(
                published(&store),
                [(A1.to_vec(), vec![Rqrr])],
                "{refusal:?}"
            );
        }
    }

    #[test]
    fn a_margin_counts_for_a_standing_read_of_the_same_content_within_the_reserve() {
        let asked = read(9, A1, NoEci, X, L);
        let measured = |read: Read| store([read], &[]).has_margin(&asked);
        assert!(measured(margin(read(0, A1, NoEci, Q, L), 0)));
        // another content within the reserve, the same one past it or not
        // measured at all: no margin to rely on
        assert!(!measured(margin(read(0, E_ACUTE, NoEci, Q, L), 0)));
        assert!(!measured(margin(read(0, A1, eci4(), Q, L), 0)));
        assert!(!measured(margin(read(0, A1, NoEci, Q, L), -1)));
        assert!(!measured(read(0, A1, NoEci, Q, L)));
        // a read the refusals withhold does not stand
        let withheld = store_with(margin(read(0, A1, Signalled, Q, L), 0), port(A1, L));
        assert!(!withheld.has_margin(&read(9, A1, Signalled, X, L)));
    }

    #[test]
    fn an_unparsed_qr_read_is_a_standing_qr_code_engine_read_no_parse_read() {
        let unparsed = |read: Read| store([read], &[]).unparsed_qr();
        assert!(unparsed(read(0, A1, NoEci, X, L)));
        assert!(unparsed(read(0, A1, NoEci, Q, L)));
        assert!(!unparsed(parsed(read(0, A1, NoEci, X, L))), "parsed");
        let mut bare = read(0, A1, NoEci, C, L);
        bare.parsed = false;
        assert!(!unparsed(bare), "no engine");
        let mut micro = read(0, A1, NoEci, X, L);
        micro.symbology = Symbology::MicroQrCode;
        assert!(!unparsed(micro), "another symbology");
        assert!(!ReadStore::default().unparsed_qr());
        let withheld = store_with(read(0, A1, Signalled, X, L), port(A1, L));
        assert!(!withheld.unparsed_qr(), "withheld");
    }

    #[test]
    fn a_read_of_unknown_evidence_joins_only_a_detection_of_its_fnc1_position() {
        // a located GS1 detection at L and an rqrr read of its bytes there:
        // unobserved evidence agrees, FNC1 none does not — two detections
        let gs1 = fnc1(parsed(read(0, A1, eci4(), X, L)), Fnc1::First);
        let found = every_order(&[gs1.clone(), read(1, A1, Unobserved, Q, L)], &[]);
        assert_eq!(engines(&found), [vec![Rxing], vec![Rqrr]]);
        // a position unknown, a located account it does not agree with: the
        // same
        let declared = fnc1(read(1, A1, NoEci, Q, L), Fnc1::Unobserved);
        let found = every_order(&[gs1, declared], &[]);
        assert_eq!(engines(&found), [vec![Rxing], vec![Rqrr]]);
    }

    #[test]
    fn reads_forming_a_detection_of_one_identity_form_one() {
        // reconciled as given: identical reads would fold in a store first
        // signalled reads of one position, no located detection: one
        let found = reconcile(vec![
            read(0, A1, Signalled, X, L),
            read(1, A1, Signalled, Q, L),
        ]);
        assert_eq!(engines(&found), [vec![Rxing, Rqrr]]);
        // of two positions: two
        let found = reconcile(vec![
            read(0, A1, Signalled, X, L),
            fnc1(read(1, A1, Signalled, Q, L), Fnc1::First),
        ]);
        assert_eq!(engines(&found), [vec![Rxing], vec![Rqrr]]);
        // unobserved reads with nothing formed before them: one per identity
        let found = reconcile(vec![
            read(0, A1, Unobserved, Q, L),
            read(1, A1, Unobserved, X, L),
        ]);
        assert_eq!(engines(&found), [vec![Rqrr, Rxing]]);
    }

    #[test]
    fn a_detection_s_engine_reading_is_its_reads_one_reading() {
        // reconciled as given: identical reads would fold in a store first
        let with = |n, engine, reading| engine_reading(read(n, YAMADA, NoEci, engine, L), reading);
        let found = reconcile(vec![
            with(0, X, EngineReading::ShiftJis),
            with(1, Q, EngineReading::ShiftJis),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].engine_reading, EngineReading::ShiftJis);
        let found = reconcile(vec![
            with(0, X, EngineReading::ShiftJis),
            with(1, Q, EngineReading::Gb18030),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].engine_reading, EngineReading::None);
        // a read without one says nothing either way
        let found = reconcile(vec![
            with(0, X, EngineReading::None),
            with(1, Q, EngineReading::Gb18030),
        ]);
        assert_eq!(found[0].engine_reading, EngineReading::Gb18030);
    }
}
