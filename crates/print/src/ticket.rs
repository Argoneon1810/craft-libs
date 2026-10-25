//! The Windows job's PrintTicket, and what is read back from the driver's: plain XML and
//! numbers, so it is tested on every platform. The calls that hand them to the driver are in
//! `windows.rs`.

use crate::{Duplex, Error, Job};

pub(crate) const NS_PSF: &str = "http://schemas.microsoft.com/windows/2003/08/printing/printschemaframework";
pub(crate) const NS_PSK: &str = "http://schemas.microsoft.com/windows/2003/08/printing/printschemakeywords";

/// Paper sizes, in micrometres, this close are the same paper (a quarter of a millimetre).
pub(crate) const PAPER_TOLERANCE: u32 = 250;

fn spool_error(e: impl std::fmt::Display) -> Error {
    Error::Spool(e.to_string())
}

/// Points to micrometres; `None` for a size no printer takes (at most 200 inches, so the
/// conversion can't overflow).
pub(crate) fn micrometres(points: f64) -> Option<u32> {
    (points.is_finite() && points > 0.0 && points <= 14_400.0).then(|| (points * 25_400.0 / 72.0).round() as u32)
}

pub(crate) fn points(micrometres: u32) -> f64 {
    f64::from(micrometres) * 72.0 / 25_400.0
}

/// The PrintTicket the job asks for: the job's options, the orientation, and no scaling (the
/// sheets are laid out at their final size; `sheets.rs` reframes them for shrink-to-fit). The
/// paper is merged in separately, as the driver's own option. Only schema keywords and bounded
/// numbers go into the XML; the printer name and the document title never do. Two-sided is left
/// out when the job doesn't set it, so the queue's default applies.
pub(crate) fn ticket_xml(job: &Job, landscape: bool) -> String {
    let orientation = if landscape { "Landscape" } else { "Portrait" };
    let duplex = match job.duplex {
        None => String::new(),
        Some(duplex) => {
            let option = match duplex {
                Duplex::Off => "OneSided",
                Duplex::LongEdge => "TwoSidedLongEdge",
                Duplex::ShortEdge => "TwoSidedShortEdge",
            };
            format!("\n <psf:Feature name=\"psk:JobDuplexAllDocumentsContiguously\"><psf:Option name=\"psk:{option}\"/></psf:Feature>")
        }
    };
    let collate = if job.collate { "Collated" } else { "Uncollated" };
    let color = if job.grayscale { "Monochrome" } else { "Color" };
    let copies = job.copies.clamp(1, 999);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<psf:PrintTicket version="1" xmlns:psf="{NS_PSF}" xmlns:psk="{NS_PSK}" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema">
 <psf:ParameterInit name="psk:JobCopiesAllDocuments"><psf:Value xsi:type="xsd:integer">{copies}</psf:Value></psf:ParameterInit>
 <psf:Feature name="psk:DocumentCollate"><psf:Option name="psk:{collate}"/></psf:Feature>{duplex}
 <psf:Feature name="psk:PageOutputColor"><psf:Option name="psk:{color}"/></psf:Feature>
 <psf:Feature name="psk:PageScaling"><psf:Option name="psk:None"/></psf:Feature>
 <psf:Feature name="psk:PageOrientation"><psf:Option name="psk:{orientation}"/></psf:Feature>
</psf:PrintTicket>"#
    )
}

/// Standard paper sizes (points, portrait) and the names drivers give them. A driver can list
/// one size twice, a plotter's "ARCH D" next to a custom 24 × 36 in entry; the named one is
/// preferred (from storytold/pdfcraft#599).
const NAMED_PAPERS: &[((f64, f64), &[&str])] = &[
    ((612.0, 792.0), &["Letter", "US Letter"]),
    ((612.0, 1008.0), &["Legal", "US Legal"]),
    ((792.0, 1224.0), &["Tabloid", "Ledger", "ANSI B"]),
    ((419.53, 595.28), &["A5"]),
    ((595.28, 841.89), &["A4"]),
    ((841.89, 1190.55), &["A3"]),
    ((1190.55, 1683.78), &["A2"]),
    ((1683.78, 2383.94), &["A1"]),
    ((2383.94, 3370.39), &["A0"]),
    ((648.0, 864.0), &["ARCH A"]),
    ((864.0, 1296.0), &["ARCH B"]),
    ((1296.0, 1728.0), &["ARCH C"]),
    ((1728.0, 2592.0), &["ARCH D"]),
    ((2160.0, 3024.0), &["ARCH E1"]),
    ((2592.0, 3456.0), &["ARCH E"]),
    ((1224.0, 1584.0), &["ANSI C"]),
    ((1584.0, 2448.0), &["ANSI D"]),
    ((2448.0, 3168.0), &["ANSI E"]),
];

/// The usual names of a paper of this size (either way round, to half a point).
pub(crate) fn paper_names(width: f64, height: f64) -> &'static [&'static str] {
    let (w, h) = (width.min(height), width.max(height));
    NAMED_PAPERS.iter().find(|((pw, ph), _)| (pw - w).abs() <= 0.5 && (ph - h).abs() <= 0.5).map_or(&[], |(_, names)| names)
}

/// Whether a driver's paper name is `name`: it starts with it, ignoring case, and the name ends
/// there ("A4 210 x 297 mm", not "A4 Extra" for "A4" or "ARCH E1" for "ARCH E").
pub(crate) fn names_paper(display: Option<&str>, name: &str) -> bool {
    let Some(display) = display else { return false };
    display.get(..name.len()).is_some_and(|start| start.eq_ignore_ascii_case(name))
        && display.get(name.len()..).is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_alphanumeric()))
}

/// A driver paper offered for the job: its size in micrometres (as the driver lists it), its
/// display name, and whether its option is a Print Schema keyword (`psk:ISOA4`) rather than a
/// vendor's own.
#[derive(Clone)]
pub(crate) struct Paper<'a> {
    pub width: u32,
    pub height: u32,
    pub name: Option<&'a str>,
    pub keyword: bool,
}

/// The driver paper for a sheet of `width` × `height` points (either way round): within a
/// quarter of a millimetre, preferring the one named like the standard size, then a Print Schema
/// keyword, then the closest; on a tie the driver's own order. Its index in `papers`.
pub(crate) fn matching_paper(papers: &[Paper<'_>], width: f64, height: f64) -> Option<usize> {
    let (w, h) = (micrometres(width.min(height))?, micrometres(width.max(height))?);
    let names = paper_names(width, height);
    let mut candidates: Vec<(usize, &Paper<'_>, u32)> = papers
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let (pw, ph) = (p.width.min(p.height), p.width.max(p.height));
            let off = pw.abs_diff(w).max(ph.abs_diff(h));
            (off <= PAPER_TOLERANCE).then_some((i, p, pw.abs_diff(w) + ph.abs_diff(h)))
        })
        .collect();
    candidates.sort_by_key(|(_, p, off)| (!names.iter().any(|n| names_paper(p.name, n)), !p.keyword, *off));
    candidates.first().map(|(i, _, _)| *i)
}

fn named(node: roxmltree::Node<'_, '_>, local: &str) -> bool {
    node.attribute("name")
        .and_then(|v| v.split_once(':'))
        .is_some_and(|(prefix, name)| name == local && node.lookup_namespace_uri(Some(prefix)) == Some(NS_PSK))
}

/// The integer value of the Print Schema element under `node` with this keyword name.
fn integer(node: roxmltree::Node<'_, '_>, tag: &str, name: &str) -> Option<u32> {
    node.descendants()
        .find(|n| n.has_tag_name((NS_PSF, tag)) && named(*n, name))
        .and_then(|n| n.children().find(|v| v.has_tag_name((NS_PSF, "Value"))))
        .and_then(|n| n.text())
        .and_then(|v| v.trim().parse::<u32>().ok())
}

/// Check the driver's validated ticket: it kept the orientation, added no scaling and has a
/// paper (when `paper` is given, that one, to a quarter of a millimetre). Returns the paper's
/// width and height in micrometres as the driver lists it. Only those schema fields are read:
/// some drivers return empty vendor parameters that a stricter parser would refuse.
pub(crate) fn verify_ticket(xml: &str, paper: Option<(u32, u32)>, landscape: bool) -> Result<(u32, u32), Error> {
    let document = roxmltree::Document::parse(xml).map_err(spool_error)?;
    let feature = |name: &str| document.root_element().children().find(|n| n.has_tag_name((NS_PSF, "Feature")) && named(*n, name));
    let media = feature("PageMediaSize").ok_or_else(|| spool_error("the printer's driver did not confirm the paper size; nothing was sent"))?;
    let size = integer(media, "ScoredProperty", "MediaSizeWidth")
        .zip(integer(media, "ScoredProperty", "MediaSizeHeight"))
        .filter(|(w, h)| *w > 0 && *h > 0)
        .ok_or_else(|| spool_error("the printer's driver gave a paper without a size (a roll?); nothing was sent"))?;
    if let Some((w, h)) = paper {
        let same = |a: (u32, u32), b: (u32, u32)| a.0.abs_diff(b.0) <= PAPER_TOLERANCE && a.1.abs_diff(b.1) <= PAPER_TOLERANCE;
        if !same(size, (w, h)) && !same(size, (h, w)) {
            return Err(spool_error("the printer's driver changed the paper size; nothing was sent (choose a paper size it has)"));
        }
    }
    let orientation = feature("PageOrientation").and_then(|f| f.children().find(|n| n.has_tag_name((NS_PSF, "Option"))));
    if !orientation.is_some_and(|n| named(n, if landscape { "Landscape" } else { "Portrait" })) {
        return Err(spool_error("the printer's driver changed the orientation; nothing was sent"));
    }
    if let Some(scaling) = feature("PageScaling") {
        // Microsoft's IPP class driver names its "None" in a namespace of its own.
        if !scaling.children().any(|n| n.has_tag_name((NS_PSF, "Option")) && n.attribute("name").and_then(|v| v.rsplit(':').next()) == Some("None")) {
            return Err(spool_error("the printer's driver would scale the sheets; nothing was sent"));
        }
    }
    Ok(size)
}

/// Confirm explicit job options survived driver validation. Collation only changes output
/// when both the document and copy count exceed one. Without a duplex request the queue's
/// default applies; without forced grayscale the driver's normal color policy applies.
pub(crate) fn verify_job_options(xml: &str, job: &Job, pages: usize) -> Result<(), Error> {
    let document = roxmltree::Document::parse(xml).map_err(spool_error)?;
    let root = document.root_element();
    let option = |name: &str| {
        root.children()
            .find(|n| n.has_tag_name((NS_PSF, "Feature")) && named(*n, name))
            .and_then(|f| f.children().find(|n| n.has_tag_name((NS_PSF, "Option"))))
    };
    let refused = |field: &str| spool_error(format!("the printer's driver did not retain the requested {field}; nothing was sent"));
    let copies = job.copies.clamp(1, 999);
    if integer(root, "ParameterInit", "JobCopiesAllDocuments") != Some(copies) {
        return Err(refused("copy count"));
    }
    if copies > 1 && pages > 1 && !option("DocumentCollate").is_some_and(|n| named(n, if job.collate { "Collated" } else { "Uncollated" })) {
        return Err(refused("collation"));
    }
    if let Some(duplex) = job.duplex {
        let wanted = match duplex {
            Duplex::Off => "OneSided",
            Duplex::LongEdge => "TwoSidedLongEdge",
            Duplex::ShortEdge => "TwoSidedShortEdge",
        };
        if !option("JobDuplexAllDocumentsContiguously").is_some_and(|n| named(n, wanted)) {
            return Err(refused("simplex/duplex setting"));
        }
    }
    if job.grayscale && !option("PageOutputColor").is_some_and(|n| named(n, "Monochrome") || named(n, "Grayscale")) {
        return Err(refused("grayscale"));
    }
    Ok(())
}

/// XML Schema instance namespace (`xsi:type`).
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
/// XML Schema datatypes namespace (`xsd:integer`, `xsd:QName`).
const NS_XSD: &str = "http://www.w3.org/2001/XMLSchema";
/// The namespace XML itself binds to the `xml` prefix.
const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";

/// A PrintTicket that sets nothing. Merged and validated by a driver it comes back as the
/// driver's own default ticket, which spells the driver's vendor namespaces the way the driver does.
pub(crate) fn empty_ticket() -> String {
    format!(r#"<psf:PrintTicket version="1" xmlns:psf="{NS_PSF}" xmlns:psk="{NS_PSK}" xmlns:xsi="{NS_XSI}" xmlns:xsd="{NS_XSD}"/>"#)
}

/// The (prefix, namespace) pairs the root element declares for namespaces other than the Print
/// Schema's: the driver's vendor namespaces.
fn vendor_prefixes(document: &roxmltree::Document<'_>) -> Vec<(String, String)> {
    document
        .root_element()
        .namespaces()
        .filter_map(|n| n.name().map(|prefix| (prefix.to_string(), n.uri().to_string())))
        .filter(|(_, uri)| ![NS_PSF, NS_PSK, NS_XSI, NS_XSD, NS_XML].contains(&uri.as_str()))
        .collect()
}

/// Whether every prefix in a `name` or `constrained` attribute or a QName value of the ticket is
/// declared: the XML parser does not check a prefix inside an attribute value.
fn prefixes_resolve(document: &roxmltree::Document<'_>) -> bool {
    let unresolved = |element: roxmltree::Node<'_, '_>, value: &str| {
        value.split_once(':').is_some_and(|(prefix, _)| element.lookup_namespace_uri(Some(prefix)).is_none())
    };
    !document.descendants().any(|node| {
        if node.is_element() {
            node.attributes().any(|a| a.namespace().is_none() && matches!(a.name(), "name" | "constrained") && unresolved(node, a.value()))
        } else {
            node.is_text() && node.parent().is_some_and(|p| is_qname_value(p) && node.text().is_some_and(|t| unresolved(p, t.trim())))
        }
    })
}

/// A `Value` whose text is a qualified name (`xsi:type="xsd:QName"`): its prefix is a namespace's.
fn is_qname_value(node: roxmltree::Node<'_, '_>) -> bool {
    node.attribute((NS_XSI, "type")).is_some_and(|t| t.rsplit(':').next() == Some("QName"))
}

/// The start of every `xmlns:prefix` declaration's prefix in the start tag that begins at
/// `start`, with the prefix.
fn declared_prefixes(xml: &str, start: usize) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut quote = None;
    let mut at = start;
    let mut previous = None;
    for c in xml.get(start..).unwrap_or_default().chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '>' => break,
            None => {}
        }
        if quote.is_none()
            && c == 'x'
            && previous.is_some_and(char::is_whitespace)
            && let Some(name) = xml.get(at..).and_then(|rest| rest.strip_prefix("xmlns:"))
        {
            let prefix: String = name.chars().take_while(|c| !c.is_whitespace() && *c != '=').collect();
            found.push((at + "xmlns:".len(), prefix));
        }
        previous = Some(c);
        at += c.len_utf8();
    }
    found
}

/// `ticket` with the driver's own prefix for each of the driver's vendor namespaces, where the
/// ticket spells one differently; `None` when nothing needs changing, or when it can't be done
/// safely (then the ticket stays as it is). `driver` is the driver's own ticket
/// ([`empty_ticket`] validated).
///
/// Windows' ticket merge names a vendor namespace with a prefix of its own (`ns0000`) as soon as the
/// merged ticket uses a name from it, such as the driver's own paper option from its capabilities.
/// The prefix means nothing in XML, but some drivers' print pipelines read it as text: the job is
/// accepted, then every page fails in the XPS print API (`0x80040003`, "No such interface
/// supported"). A Brother driver names its namespace `brpsk` and fails with `ns0000`. Only the
/// declarations and the vendor names in `name`/`constrained` attributes and QName values change.
/// A ticket is left as it is when it redeclares one of the prefixes below its root, or has an
/// element or attribute named in a namespace whose prefix would change.
pub(crate) fn respell(ticket: &str, driver: &str) -> Option<String> {
    let document = roxmltree::Document::parse(ticket).ok()?;
    let own = vendor_prefixes(&roxmltree::Document::parse(driver).ok()?);
    let declared = vendor_prefixes(&document);
    let renames: Vec<(&str, &str)> = declared
        .iter()
        .filter_map(|(old, uri)| own.iter().find(|(_, u)| u == uri).map(|(new, _)| (old.as_str(), new.as_str())))
        .filter(|(old, new)| old != new)
        .collect();
    if renames.is_empty() {
        return None;
    }
    let renamed = |old: &str| renames.iter().find(|(o, _)| *o == old).map(|(_, new)| *new);
    // Each prefix must still name one namespace; a swap of two (ns0000 and ns0001) is fine.
    let mut after: Vec<&str> = document.root_element().namespaces().filter_map(|n| n.name()).map(|p| renamed(p).unwrap_or(p)).collect();
    let declared_count = after.len();
    after.sort_unstable();
    after.dedup();
    if after.len() != declared_count {
        return None;
    }
    // Only the root's declarations are rewritten: an element below it that declares either prefix
    // again would make a name mean something else once the prefixes change.
    let touched = |prefix: &str| renames.iter().any(|(old, new)| prefix == *old || prefix == *new);
    let redeclared = document
        .descendants()
        .filter(|n| n.is_element() && *n != document.root_element())
        .any(|n| declared_prefixes(ticket, n.range().start).iter().any(|(_, prefix)| touched(prefix)));
    if redeclared {
        return None;
    }
    // Changing the root's declarations moves every name written with one of these prefixes, and only
    // the `name`/`constrained` values and QName values are rewritten to follow. An element or attribute
    // name (`<ns0000:Feature/>`, `ns0000:flag="1"`) would not follow: it would be left with an undeclared
    // prefix, or, when two namespaces trade prefixes, quietly resolve to the other namespace. Names
    // in a namespace whose prefix stays as it is are not affected.
    let moved: Vec<&str> = document.root_element().namespaces().filter(|n| n.name().is_some_and(touched)).map(|n| n.uri()).collect();
    let names_moved = document.descendants().filter(|n| n.is_element()).any(|n| {
        n.tag_name().namespace().is_some_and(|uri| moved.contains(&uri))
            || n.attributes().any(|a| a.namespace().is_some_and(|uri| moved.contains(&uri)))
    });
    if names_moved {
        return None;
    }
    let respelled = |at: usize| {
        let rest = ticket.get(at..)?;
        renames.iter().find(|(old, _)| rest.strip_prefix(old).is_some_and(|r| r.starts_with(':'))).map(|(old, new)| (at, at + old.len(), *new))
    };
    let mut edits: Vec<(usize, usize, &str)> = Vec::new();
    for (at, prefix) in declared_prefixes(ticket, document.root_element().range().start) {
        if let Some(new) = renamed(&prefix) {
            edits.push((at, at + prefix.len(), new));
        }
    }
    for node in document.descendants() {
        if node.is_element() {
            for a in
                node.attributes().filter(|a| (a.namespace().is_none() && matches!(a.name(), "name" | "constrained")) || a.namespace() == Some(NS_XSI))
            {
                edits.extend(respelled(a.range_value().start));
            }
        } else if node.is_text() && node.parent().is_some_and(is_qname_value) {
            edits.extend(respelled(node.range().start));
        }
    }
    edits.sort_unstable_by_key(|(start, ..)| *start);
    let mut out = String::with_capacity(ticket.len());
    let mut copied = 0;
    for (start, end, new) in edits {
        out.push_str(ticket.get(copied..start)?);
        out.push_str(new);
        copied = end;
    }
    out.push_str(ticket.get(copied..)?);
    // A last check on the result: it must parse, declare each namespace under the prefix wanted and
    // resolve every prefix.
    let check = roxmltree::Document::parse(&out).ok()?;
    let mut now = vendor_prefixes(&check);
    let mut wanted: Vec<(String, String)> = declared.iter().map(|(old, uri)| (renamed(old).unwrap_or(old).to_string(), uri.clone())).collect();
    now.sort_unstable();
    wanted.sort_unstable();
    (now == wanted && prefixes_resolve(&check)).then_some(out)
}

/// Each vendor namespace that `ticket` declares under a prefix the driver does not declare it with,
/// as (namespace, prefix in the ticket, prefix of the driver); empty when the ticket is spelled the
/// way the driver spells it. `driver` is the driver's own ticket ([`empty_ticket`] validated).
/// `None` when either does not parse.
///
/// For checking the ticket a job ends up with. [`respell`] returning `None` for it does not: that
/// is also what it returns when it refused to change a ticket, which then stays as it is.
#[cfg(test)]
pub(crate) fn prefixes_unlike_the_drivers(ticket: &str, driver: &str) -> Option<Vec<(String, String, String)>> {
    let own = vendor_prefixes(&roxmltree::Document::parse(driver).ok()?);
    let declared = vendor_prefixes(&roxmltree::Document::parse(ticket).ok()?);
    Some(
        declared
            .into_iter()
            .filter(|pair| !own.contains(pair))
            .filter_map(|(prefix, uri)| {
                let (theirs, _) = own.iter().find(|(_, u)| *u == uri)?;
                Some((uri, prefix, theirs.clone()))
            })
            .collect(),
    )
}

/// The widest unprintable margin of the paper, in micrometres, from PrintCapabilities computed
/// for the job's ticket (`PageImageableSize`). The largest of the four, so content centred inside
/// it is clear of every edge whichever way the driver measures. `None` when the driver doesn't say.
pub(crate) fn imageable_margin(capabilities_xml: &str) -> Option<u32> {
    let document = roxmltree::Document::parse(capabilities_xml).ok()?;
    let imageable = document.root_element().children().find(|n| n.has_tag_name((NS_PSF, "Property")) && named(*n, "PageImageableSize"))?;
    let width = integer(imageable, "Property", "ImageableSizeWidth")?;
    let height = integer(imageable, "Property", "ImageableSizeHeight")?;
    let area = imageable.children().find(|n| n.has_tag_name((NS_PSF, "Property")) && named(*n, "ImageableArea"))?;
    let (x, y) = (integer(area, "Property", "OriginWidth")?, integer(area, "Property", "OriginHeight")?);
    let (w, h) = (integer(area, "Property", "ExtentWidth")?, integer(area, "Property", "ExtentHeight")?);
    let right = width.checked_sub(x)?.checked_sub(w)?;
    let bottom = height.checked_sub(y)?.checked_sub(h)?;
    Some(x.max(y).max(right).max(bottom))
}

/// The job file's name, which the spooler shows as the document's: the title, with anything that
/// could leave the folder replaced and Windows' device names (`CON`, `LPT1`) avoided.
pub(crate) fn job_filename(title: &str) -> String {
    let stem = title.strip_suffix(".pdf").or_else(|| title.strip_suffix(".PDF")).unwrap_or(title);
    let safe: String =
        stem.chars().take(100).map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') { c } else { '_' }).collect();
    // Windows drops trailing dots and spaces from a name.
    let safe = safe.trim_end_matches(['.', ' ']).trim_start();
    let device = safe.split('.').next().map(str::trim_end).is_some_and(|base| {
        let base = base.to_ascii_uppercase();
        ["CON", "PRN", "AUX", "NUL"].contains(&base.as_str())
            || ((base.starts_with("COM") || base.starts_with("LPT")) && base.len() == 4 && base.ends_with(|c: char| c.is_ascii_digit()))
    });
    match (safe.is_empty(), device) {
        (true, _) => "Print.pdf".into(),
        (false, true) => format!("_{safe}.pdf"),
        (false, false) => format!("{safe}.pdf"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ticket_carries_the_job_options() {
        let job = Job { copies: 3, collate: false, duplex: Some(Duplex::ShortEdge), grayscale: true, title: "<&>".into(), ..Job::default() };
        let xml = ticket_xml(&job, true);
        for value in [">3<", "psk:Uncollated", "psk:TwoSidedShortEdge", "psk:Monochrome", "psk:Landscape", "psk:None"] {
            assert!(xml.contains(value), "missing {value}");
        }
        assert!(!xml.contains("<&>"), "the title never enters the XML");
        assert!(roxmltree::Document::parse(&xml).is_ok());
        let xml = ticket_xml(&Job { copies: 5000, ..Job::default() }, false);
        for value in [">999<", "psk:Collated", "psk:Color", "psk:Portrait"] {
            assert!(xml.contains(value), "missing {value}");
        }
        assert!(!xml.contains("JobDuplex"), "no duplex asked: the queue's default applies");
        assert!(ticket_xml(&Job { duplex: Some(Duplex::Off), ..Job::default() }, false).contains("psk:OneSided"));
        assert!(ticket_xml(&Job { duplex: Some(Duplex::LongEdge), ..Job::default() }, false).contains("psk:TwoSidedLongEdge"));
        assert!(!ticket_xml(&Job { printer: Some("Office \"east\" <1>".into()), ..Job::default() }, false).contains("east"));
    }

    /// A validated ticket as a driver returns it.
    fn validated(paper: (u32, u32), orientation: &str, scaling: Option<&str>) -> String {
        let scaling = scaling.map_or(String::new(), |s| format!(r#"<psf:Feature name="psk:PageScaling"><psf:Option name="{s}"/></psf:Feature>"#));
        format!(
            r#"<psf:PrintTicket version="1" xmlns:psf="{NS_PSF}" xmlns:psk="{NS_PSK}" xmlns:ns0000="http://example.com/vendor" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema">
<psf:Feature name="psk:PageMediaSize"><psf:Option name="psk:NorthAmericaLetter">
<psf:ScoredProperty name="psk:MediaSizeWidth"><psf:Value xsi:type="xsd:integer">{}</psf:Value></psf:ScoredProperty>
<psf:ScoredProperty name="psk:MediaSizeHeight"><psf:Value xsi:type="xsd:integer">{}</psf:Value></psf:ScoredProperty>
</psf:Option></psf:Feature>
<psf:Feature name="psk:PageOrientation"><psf:Option name="psk:{orientation}"/></psf:Feature>{scaling}
<psf:ParameterInit name="ns0000:Empty"><psf:Value xsi:type="xsd:string"></psf:Value></psf:ParameterInit>
</psf:PrintTicket>"#,
            paper.0, paper.1
        )
    }

    #[test]
    fn a_driver_that_changes_the_sheet_is_refused() {
        let letter = (215_900, 279_400);
        let ticket = validated(letter, "Portrait", Some("psk:None"));
        assert_eq!(verify_ticket(&ticket, Some(letter), false), Ok(letter));
        assert_eq!(verify_ticket(&ticket, Some((279_400, 215_900)), false), Ok(letter), "either way round");
        assert_eq!(verify_ticket(&ticket, None, false), Ok(letter), "shrink-to-fit takes the driver's paper");
        assert!(verify_ticket(&ticket, Some((210_000, 297_000)), false).is_err(), "A4 asked, Letter returned");
        assert!(verify_ticket(&ticket, Some(letter), true).is_err(), "landscape asked, portrait returned");
        assert!(verify_ticket(&validated(letter, "Portrait", Some("psk:FitApplicationBleedSizeToPageImageableSize")), Some(letter), false).is_err());
        assert!(verify_ticket(&validated(letter, "Portrait", Some("ns0000:None")), Some(letter), false).is_ok(), "the IPP class driver's None");
        assert!(verify_ticket(&validated(letter, "Portrait", None), Some(letter), false).is_ok(), "a driver without scaling");
        assert!(verify_ticket(&validated((0, 279_400), "Portrait", None), None, false).is_err(), "a roll has no length");
        assert!(verify_ticket("<not xml", Some(letter), false).is_err());
    }

    /// The namespace of the Brother driver's own keywords.
    const BROTHER: &str = "http://schemas.brother.info/mfc/printing/2006/11/printschemakeywords";

    /// A Brother driver's ticket with its vendor namespace declared as `prefix`. The driver's own
    /// spelling is `brpsk`; Windows' ticket merge returned `ns0000` for the same ticket, and the
    /// printer then failed every page (`0x80040003`).
    fn brother(prefix: &str) -> String {
        format!(
            r#"<psf:PrintTicket xmlns:psf="{NS_PSF}" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" version="1" xmlns:psk="{NS_PSK}" xmlns:{prefix}="{BROTHER}"><psf:Feature name="psk:PageMediaSize"><psf:Option name="psk:NorthAmericaLetter"><psf:ScoredProperty name="psk:MediaSizeWidth"><psf:Value xsi:type="xsd:integer">215900</psf:Value></psf:ScoredProperty><psf:ScoredProperty name="psk:MediaSizeHeight"><psf:Value xsi:type="xsd:integer">279400</psf:Value></psf:ScoredProperty></psf:Option></psf:Feature><psf:Feature name="psk:PageOrientation"><psf:Option name="psk:Portrait"/></psf:Feature><psf:Property name="{prefix}:PageMediaSizeData"><psf:Property name="{prefix}:CustomUnitType"><psf:Value xsi:type="xsd:string">mm</psf:Value></psf:Property></psf:Property><psf:Feature name="{prefix}:JobPrintSpeed"><psf:Option name="{prefix}:PrinterDefault"/></psf:Feature><psf:ParameterInit name="{prefix}:Mode"><psf:Value xsi:type="xsd:QName">{prefix}:Fast</psf:Value></psf:ParameterInit><psf:ParameterInit name="{prefix}:Label"><psf:Value xsi:type="xsd:string">ns0000:text, not a name</psf:Value></psf:ParameterInit></psf:PrintTicket>"#
        )
    }

    /// The ticket Windows returns (`ns0000`) gets the driver's own `brpsk` back, in the declaration, the names and the QName values, but not in text.
    #[test]
    fn a_vendor_namespace_gets_the_prefix_the_driver_declares_it_with() {
        let merged = brother("ns0000");
        assert_eq!(respell(&merged, &brother("brpsk")), Some(brother("brpsk")), "declaration, names and QName values; not the text");
        assert_eq!(respell(&brother("brpsk"), &brother("brpsk")), None, "already the driver's");
        assert_eq!(respell(&brother("brpsk"), &merged), Some(merged.clone()), "whatever the driver calls it");
        // What the job's checks read is the same either way.
        let letter = (215_900, 279_400);
        assert_eq!(verify_ticket(&merged, Some(letter), false), Ok(letter));
        assert_eq!(verify_ticket(&respell(&merged, &brother("brpsk")).unwrap(), Some(letter), false), Ok(letter));
    }

    /// A second vendor namespace, next to [`BROTHER`].
    const OTHER: &str = "http://example.com/other";

    /// A ticket that names [`BROTHER`] with the prefix `a` and [`OTHER`] with `b` in its root, uses
    /// each in a `name`, and ends with `extra`.
    fn two_namespaces(a: &str, b: &str, extra: &str) -> String {
        format!(
            r#"<psf:PrintTicket xmlns:psf="{NS_PSF}" xmlns:psk="{NS_PSK}" xmlns:{a}="{BROTHER}" xmlns:{b}="{OTHER}"><psf:Feature name="{a}:One"/><psf:Feature name="{b}:Two"/>{extra}</psf:PrintTicket>"#
        )
    }

    /// Prefixes are renamed together, so two namespaces that swap prefixes still end up distinct.
    #[test]
    fn two_vendor_namespaces_can_trade_prefixes() {
        let ticket = |a: &str, b: &str| two_namespaces(a, b, "");
        assert_eq!(respell(&ticket("ns0000", "ns0001"), &ticket("ns0001", "ns0000")), Some(ticket("ns0001", "ns0000")));
    }

    /// When two namespaces trade prefixes both prefixes stay declared, so a name that is only
    /// written with a prefix (an element or an attribute, unlike a `name` value) would still parse and
    /// resolve, but to the other namespace. Each ticket here is well-formed and has every prefix
    /// declared, so `respell` leaves it alone for the name, not because the result didn't parse.
    #[test]
    fn a_swap_does_not_move_an_element_or_attribute_name_to_the_other_namespace() {
        let driver = two_namespaces("ns0001", "ns0000", "");
        let refused = |extra: &str| {
            let ticket = two_namespaces("ns0000", "ns0001", extra);
            let document = roxmltree::Document::parse(&ticket).expect("the fixture must stay well-formed");
            assert!(prefixes_resolve(&document));
            respell(&ticket, &driver)
        };
        assert!(refused("").is_some(), "control: the same ticket without such a name is respelled");

        // An element: `ns0000:Feature` is BROTHER's until the declarations swap, then OTHER's.
        assert_eq!(refused("<ns0000:Feature/>"), None, "element in the first namespace");
        assert_eq!(refused("<ns0001:Feature/>"), None, "element in the second namespace");
        assert_eq!(refused("<psf:Property name=\"ns0001:Three\"><ns0000:Child/></psf:Property>"), None, "nested element");

        // An attribute, whichever element carries it.
        assert_eq!(refused("<psf:Feature name=\"ns0000:Three\" ns0000:flag=\"1\"/>"), None, "attribute in the first namespace");
        assert_eq!(refused("<psf:Feature name=\"ns0000:Three\" ns0001:flag=\"1\"/>"), None, "attribute in the second namespace");
        assert_eq!(refused("<ns0001:Feature ns0000:flag=\"1\"/>"), None, "both at once");
    }

    /// The same when the prefixes form a chain rather than a swap: `brpsk` is OTHER's, is about to
    /// become BROTHER's, and OTHER moves on to `ns0005`.
    #[test]
    fn a_name_in_a_namespace_whose_prefix_is_taken_over_is_not_respelled() {
        let driver = two_namespaces("brpsk", "ns0005", "");
        let refused = |extra: &str| {
            let ticket = two_namespaces("ns0000", "brpsk", extra);
            let document = roxmltree::Document::parse(&ticket).expect("the fixture must stay well-formed");
            assert!(prefixes_resolve(&document));
            respell(&ticket, &driver)
        };
        assert!(refused("").is_some(), "control");
        assert_eq!(refused("<brpsk:Feature/>"), None, "element: OTHER's now, BROTHER's after");
        assert_eq!(refused("<psf:Feature brpsk:flag=\"1\"/>"), None, "attribute");
    }

    /// What the live driver test checks. A ticket `respell` refused keeps the prefix the merge gave
    /// it, but `respell` says `None` for it, as it does for a ticket that is already right: only the
    /// prefixes tell the two apart.
    #[test]
    fn the_prefixes_tell_a_refused_ticket_from_one_in_the_drivers_spelling() {
        let driver = brother("brpsk");
        let unlike = |ticket: &str| prefixes_unlike_the_drivers(ticket, &driver).expect("both tickets parse");
        let merged = brother("ns0000");
        let mismatch = (BROTHER.to_string(), "ns0000".to_string(), "brpsk".to_string());

        assert_eq!(unlike(&merged), std::slice::from_ref(&mismatch), "as the merge returns it");
        assert_eq!(unlike(&respell(&merged, &driver).unwrap()), [], "respelled");
        assert_eq!(unlike(&driver), [], "already the driver's");

        // `respell` refuses this one (`ns0000` is declared again below the root), so it stays as the
        // merge left it, and `respell` says `None` for it just as it does for the driver's own.
        let refused = merged.replace(
            "<psf:Feature name=\"ns0000:JobPrintSpeed\">",
            "<psf:Feature name=\"ns0000:JobPrintSpeed\" xmlns:ns0000=\"http://example.com/nested\">",
        );
        assert_ne!(refused, merged);
        assert_eq!(respell(&refused, &driver), None);
        assert_eq!(respell(&driver, &driver), None);
        assert_eq!(unlike(&refused), [mismatch], "refused, and still wrong");

        // Two namespaces that trade prefixes are both reported.
        let traded = prefixes_unlike_the_drivers(&two_namespaces("ns0000", "ns0001", ""), &two_namespaces("ns0001", "ns0000", "")).unwrap();
        assert_eq!(traded.len(), 2, "{traded:?}");
        // A namespace the driver doesn't declare has no prefix of the driver's to differ from.
        assert_eq!(unlike(&empty_ticket()), [], "no vendor namespace to compare");
        assert_eq!(unlike(&two_namespaces("ns0000", "ns0001", "")).len(), 1, "only BROTHER is the driver's");
        assert_eq!(prefixes_unlike_the_drivers("<not xml", &driver), None);
        assert_eq!(prefixes_unlike_the_drivers(&merged, "<not xml"), None);
    }

    /// Only names in a namespace whose prefix changes block the respelling.
    #[test]
    fn names_in_a_namespace_that_keeps_its_prefix_do_not_block_the_respelling() {
        let keep = "http://example.com/keep";
        let ticket = |prefix: &str| {
            format!(
                r#"<psf:PrintTicket xmlns:psf="{NS_PSF}" xmlns:psk="{NS_PSK}" xmlns:{prefix}="{BROTHER}" xmlns:keep="{keep}"><psf:Feature name="{prefix}:One" keep:flag="1"><keep:Child keep:flag="2"/></psf:Feature><psf:Feature xmlns:own="http://example.com/own" own:flag="3"/></psf:PrintTicket>"#
            )
        };
        assert_eq!(respell(&ticket("ns0000"), &ticket("brpsk")), Some(ticket("brpsk")));
    }

    /// Each ticket here is well-formed XML, so `respell` leaves it alone for the reason named, not
    /// because it didn't parse.
    #[test]
    fn a_ticket_that_cannot_be_respelled_safely_is_left_alone() {
        let merged = brother("ns0000");
        let driver = brother("brpsk");
        let refused = |ticket: &str| {
            assert!(roxmltree::Document::parse(ticket).is_ok(), "the fixture must stay well-formed");
            respell(ticket, &driver)
        };
        assert_eq!(respell(&merged, &empty_ticket()), None, "the driver has no such namespace");
        assert_eq!(respell("<not xml", &driver), None);
        assert_eq!(respell(&merged, "<not xml"), None);
        // `brpsk` is already another namespace's prefix in the ticket.
        assert_eq!(refused(&merged.replace("xmlns:psk=", "xmlns:brpsk=\"http://example.com/other\" xmlns:psk=")), None);
        // A vendor element can't be renamed by changing attributes and declarations.
        let feature = "<psf:Feature name=\"ns0000:JobPrintSpeed\"><psf:Option name=\"ns0000:PrinterDefault\"/></psf:Feature>";
        assert!(merged.contains(feature));
        assert_eq!(refused(&merged.replace(feature, "<ns0000:Feature/>")), None);
        // A name whose prefix is not declared stays refused after the change.
        assert_eq!(refused(&merged.replace("name=\"ns0000:Mode\"", "name=\"ns0001:Mode\"")), None);
        // A nested element that declares either prefix again would change what a name resolves to.
        for redeclared in ["ns0000", "brpsk"] {
            let nested = merged.replace(
                "<psf:Feature name=\"ns0000:JobPrintSpeed\">",
                &format!("<psf:Feature name=\"ns0000:JobPrintSpeed\" xmlns:{redeclared}=\"http://example.com/nested\">"),
            );
            assert_ne!(nested, merged);
            assert_eq!(refused(&nested), None, "{redeclared} declared again below the root");
        }
        // Control: the same ticket without the nested declaration is respelled.
        assert!(refused(&merged).is_some());
    }

    #[test]
    fn a_driver_that_drops_explicit_job_options_is_refused() {
        let job = Job { copies: 2, duplex: Some(Duplex::LongEdge), grayscale: true, ..Job::default() };
        let xml = ticket_xml(&job, false);
        assert!(verify_job_options(&xml, &job, 3).is_ok());
        for (from, to) in
            [(">2<", ">1<"), ("psk:Collated", "psk:Uncollated"), ("psk:TwoSidedLongEdge", "psk:OneSided"), ("psk:Monochrome", "psk:Color")]
        {
            assert!(verify_job_options(&xml.replace(from, to), &job, 3).is_err(), "{from}");
        }
        assert!(verify_job_options(&xml.replace("psk:Collated", "vendor:Collated"), &job, 3).is_err(), "namespace matters");
        assert!(verify_job_options(&xml.replace("psk:Monochrome", "psk:Grayscale"), &job, 3).is_ok());
        for feature in ["DocumentCollate", "JobDuplexAllDocumentsContiguously", "PageOutputColor"] {
            let start = xml.find(&format!("<psf:Feature name=\"psk:{feature}\"")).unwrap();
            let end = start + xml[start..].find("</psf:Feature>").unwrap() + "</psf:Feature>".len();
            let mut dropped = xml.clone();
            dropped.replace_range(start..end, "");
            assert!(verify_job_options(&dropped, &job, 3).is_err(), "{feature}");
            if feature == "DocumentCollate" {
                assert!(verify_job_options(&dropped, &job, 1).is_ok(), "one page has no collating order");
            }
        }
        let default = Job::default();
        let xml = ticket_xml(&default, false);
        assert!(verify_job_options(&xml, &default, 3).is_ok());
        let simplex = Job { duplex: Some(Duplex::Off), ..Job::default() };
        assert!(verify_job_options(&ticket_xml(&simplex, false), &simplex, 3).is_ok());
        assert!(verify_job_options(&xml, &simplex, 3).is_err(), "explicit simplex must be confirmed");
        assert!(verify_job_options("<not xml", &default, 1).is_err());
    }

    #[test]
    fn the_driver_paper_named_like_the_standard_size_is_preferred() {
        let paper = |width, height, name, keyword| Paper { width, height, name, keyword };
        // A plotter listing 24 × 36 in three times: a custom entry first, "ARCH D", and one a hair off.
        let papers = [
            paper(609_600, 914_400, Some("Custom 24x36"), false),
            paper(609_700, 914_400, Some("ARCH D 24 x 36 in"), false),
            paper(609_600, 914_400, Some("24 x 36"), true),
        ];
        assert_eq!(matching_paper(&papers, 2592.0, 1728.0), Some(1), "named, either way round");
        assert_eq!(matching_paper(&papers[..1], 1728.0, 2592.0), Some(0));
        assert_eq!(matching_paper(&[papers[0].clone(), papers[2].clone()], 1728.0, 2592.0), Some(1), "then a schema keyword");
        assert_eq!(matching_paper(&papers, 1728.0, 2600.0), None, "3 mm off is another paper");
        assert_eq!(matching_paper(&papers, f64::NAN, 2592.0), None);
        assert_eq!(paper_names(595.0, 842.0), ["A4"], "rounded metric points");
        assert_eq!(paper_names(792.0, 612.0), ["Letter", "US Letter"]);
        assert!(paper_names(360.0, 504.0).is_empty());
        assert!(names_paper(Some("ARCH D 24 x 36 in"), "ARCH D"));
        assert!(names_paper(Some("arch d"), "ARCH D"));
        assert!(!names_paper(Some("ANSI D"), "ARCH D"));
        assert!(!names_paper(Some("ARCH E1"), "ARCH E"));
        assert!(!names_paper(Some("A4Extra"), "A4"));
        assert!(!names_paper(Some("Ä"), "A4"), "never slices inside a character");
        assert!(!names_paper(None, "A4"));
    }

    #[test]
    fn the_unprintable_margin_comes_from_the_capabilities() {
        let caps = |x: u32, y: u32, w: u32, h: u32| {
            format!(
                r#"<psf:PrintCapabilities version="1" xmlns:psf="{NS_PSF}" xmlns:psk="{NS_PSK}" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema">
<psf:Property name="psk:PageImageableSize">
 <psf:Property name="psk:ImageableSizeWidth"><psf:Value xsi:type="xsd:integer">215900</psf:Value></psf:Property>
 <psf:Property name="psk:ImageableSizeHeight"><psf:Value xsi:type="xsd:integer">279400</psf:Value></psf:Property>
 <psf:Property name="psk:ImageableArea">
  <psf:Property name="psk:OriginWidth"><psf:Value xsi:type="xsd:integer">{x}</psf:Value></psf:Property>
  <psf:Property name="psk:OriginHeight"><psf:Value xsi:type="xsd:integer">{y}</psf:Value></psf:Property>
  <psf:Property name="psk:ExtentWidth"><psf:Value xsi:type="xsd:integer">{w}</psf:Value></psf:Property>
  <psf:Property name="psk:ExtentHeight"><psf:Value xsi:type="xsd:integer">{h}</psf:Value></psf:Property>
 </psf:Property>
</psf:Property>
</psf:PrintCapabilities>"#
            )
        };
        assert_eq!(imageable_margin(&caps(4_233, 4_233, 207_434, 270_934)), Some(4_233));
        assert_eq!(imageable_margin(&caps(3_000, 5_000, 209_900, 270_000)), Some(5_000), "the widest of the four");
        assert_eq!(imageable_margin(&caps(0, 0, 215_900, 279_400)), Some(0), "borderless");
        assert_eq!(imageable_margin(&caps(10_000, 0, 215_900, 279_400)), None, "an area past the paper is not believed");
        assert_eq!(imageable_margin("<psf:PrintCapabilities/>"), None);
    }

    #[test]
    fn points_and_micrometres_convert_both_ways() {
        assert_eq!(micrometres(612.0), Some(215_900));
        assert_eq!(micrometres(841.89), Some(297_000));
        assert!((points(215_900) - 612.0).abs() < 1e-9);
        assert!((points(4_064) - 11.52).abs() < 1e-9, "a 4 mm hardware margin");
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 14_401.0] {
            assert_eq!(micrometres(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_job_file_stays_in_its_folder() {
        let name = job_filename(r"..\..\CON:/unsafe?name");
        assert_eq!(std::path::Path::new(&name).components().count(), 1, "{name}");
        assert!(name.ends_with(".pdf"));
        assert_eq!(job_filename("Invoice 42.pdf"), "Invoice 42.pdf");
        assert_eq!(job_filename("Report (draft) v1.2"), "Report (draft) v1.2.pdf");
        assert_eq!(job_filename("con"), "_con.pdf");
        assert_eq!(job_filename("LPT1.txt"), "_LPT1.txt.pdf");
        assert_eq!(job_filename("COMPLETE"), "COMPLETE.pdf");
        assert_eq!(job_filename("..."), "Print.pdf");
        assert_eq!(job_filename(""), "Print.pdf");
        assert_eq!(job_filename("Отчёт"), "Отчёт.pdf");
    }
}
