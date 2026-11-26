use super::AlertJob;
use reqwest::Url;
use scraper::{ElementRef, Html, Selector, node::Node};
use std::collections::{HashMap, HashSet};

pub(super) fn normalized(value: &str) -> String {
    value
        .replace(['–', '—'], "-")
        .replace('’', "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(super) fn is_placeholder_title(title: &str) -> bool {
    let value = normalized(title);
    value.is_empty()
        || matches!(
            value.as_str(),
            "linkedin job alert"
                | "view job"
                | "view jobs"
                | "see job"
                | "see jobs"
                | "apply"
                | "apply now"
                | "查看職缺"
                | "查看工作"
                | "立即應徵"
        )
}

fn linkedin_host(host: &str) -> bool {
    host == "linkedin.com" || host.ends_with(".linkedin.com")
}

pub(super) fn canonical_linkedin_url(value: &str) -> Option<(String, String)> {
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !linkedin_host(url.host_str()?) {
        return None;
    }
    let segments: Vec<_> = url.path_segments()?.collect();
    let position = segments
        .windows(2)
        .position(|pair| pair == ["jobs", "view"])?;
    let segment = *segments.get(position + 2)?;
    let id = segment.rsplit('-').next()?;
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((
        id.into(),
        format!("https://www.linkedin.com/jobs/view/{id}/"),
    ))
}

fn select<'a>(root: ElementRef<'a>, selector: &str) -> Vec<ElementRef<'a>> {
    Selector::parse(selector)
        .ok()
        .map(|selector| root.select(&selector).collect())
        .unwrap_or_default()
}

fn text(element: ElementRef<'_>) -> String {
    element
        .text()
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn lines(element: ElementRef<'_>) -> Vec<String> {
    let mut rendered = String::new();
    for node in element.descendants() {
        match node.value() {
            Node::Text(text) => rendered.push_str(text),
            Node::Element(element)
                if matches!(element.name(), "br" | "p" | "div" | "li" | "tr") =>
            {
                rendered.push('\n')
            }
            _ => {}
        }
    }
    rendered
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect()
}

fn card_for_link<'a>(link: ElementRef<'a>, id: &str) -> ElementRef<'a> {
    let mut card = link;
    for parent in link.ancestors().filter_map(ElementRef::wrap) {
        if !matches!(
            parent.value().name(),
            "td" | "tr" | "div" | "li" | "section" | "article"
        ) {
            continue;
        }
        let ids: HashSet<_> = select(parent, "a[href]")
            .into_iter()
            .filter_map(|anchor| canonical_linkedin_url(anchor.value().attr("href")?))
            .map(|(id, _)| id)
            .collect();
        if ids.len() != 1 || !ids.contains(id) {
            break;
        }
        if text(parent).chars().count() > 12_000 {
            break;
        }
        card = parent;
    }
    card
}

fn useful_label(value: &str, title: &str) -> bool {
    let value = normalized(value);
    !is_placeholder_title(&value)
        && value != normalized(title)
        && value.chars().count() < 200
        && ![
            "unsubscribe",
            "recommended for you",
            "recommended jobs",
            "actively recruiting",
            "school alumni",
            "applicants",
            "posted ",
            "promoted",
            "取消訂閱",
            "推薦職缺",
        ]
        .iter()
        .any(|word| value.contains(word))
}

fn looks_like_location(value: &str) -> bool {
    let lower = normalized(value);
    let last = lower.rsplit(',').next().unwrap_or(&lower).trim();
    if matches!(
        last,
        "inc" | "inc." | "ltd" | "ltd." | "llc" | "corp" | "corp." | "limited"
    ) || lower.ends_with("公司")
    {
        return false;
    }
    value.contains([',', '市', '縣'])
        || matches!(
            lower.as_str(),
            "remote" | "hybrid" | "taiwan" | "taipei" | "taipei city" | "hsinchu" | "singapore"
        )
        || lower.starts_with("remote ")
        || lower.contains("遠端")
}

fn metadata(card: ElementRef<'_>, title: &str) -> (Option<String>, Option<String>) {
    let mut company = card
        .value()
        .attr("data-company")
        .map(str::to_owned)
        .or_else(|| {
            select(
                card,
                "[data-company], [class*='company-name'], [class*='companyName']",
            )
            .into_iter()
            .find_map(|element| {
                let label = element
                    .value()
                    .attr("data-company")
                    .map(str::to_owned)
                    .unwrap_or_else(|| text(element));
                useful_label(&label, title).then_some(label)
            })
        });
    if company.is_none() {
        company = select(card, "a[href]").into_iter().find_map(|anchor| {
            let url = Url::parse(anchor.value().attr("href")?).ok()?;
            let label = text(anchor);
            (linkedin_host(url.host_str()?)
                && url.path().starts_with("/company/")
                && useful_label(&label, title))
            .then_some(label)
        });
    }
    let mut location = card
        .value()
        .attr("data-location")
        .map(str::to_owned)
        .or_else(|| {
            select(card, "[data-location], [class*='location']")
                .into_iter()
                .find_map(|element| {
                    let label = element
                        .value()
                        .attr("data-location")
                        .map(str::to_owned)
                        .unwrap_or_else(|| text(element));
                    useful_label(&label, title).then_some(label)
                })
        });
    let mut labels = Vec::new();
    for element in select(card, "p, span, td, div") {
        if !select(element, "a[href]").is_empty()
            || element
                .children()
                .filter_map(ElementRef::wrap)
                .any(|child| matches!(child.value().name(), "p" | "span" | "td" | "div"))
        {
            continue;
        }
        for label in lines(element) {
            if !useful_label(&label, title) {
                continue;
            }
            if let Some((left, right)) = label.split_once(['·', '•'])
                && looks_like_location(right)
                && useful_label(left.trim(), title)
            {
                company.get_or_insert_with(|| left.trim().to_owned());
                location.get_or_insert_with(|| right.trim().to_owned());
                continue;
            }
            labels.push(label);
        }
    }
    if let Some(index) = labels.iter().position(|label| looks_like_location(label)) {
        location.get_or_insert_with(|| labels[index].clone());
        if index > 0 && company.is_none() && !looks_like_location(&labels[index - 1]) {
            company = Some(labels[index - 1].clone());
        }
    }
    (
        company.filter(|value| !value.trim().is_empty()),
        location.filter(|value| !value.trim().is_empty()),
    )
}

fn outbound_url(value: &str) -> Option<Url> {
    let mut url = Url::parse(value).ok()?;
    for _ in 0..3 {
        if !linkedin_host(url.host_str()?) {
            break;
        }
        let target = url
            .query_pairs()
            .find(|(key, _)| key == "url")
            .map(|(_, value)| value.into_owned())?;
        url = Url::parse(&target).ok()?;
    }
    if url.scheme() != "https"
        || linkedin_host(url.host_str()?)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

fn career_url(card: ElementRef<'_>) -> Option<String> {
    let urls: HashSet<_> = select(card, "a[href]")
        .into_iter()
        .filter_map(|anchor| {
            let url = outbound_url(anchor.value().attr("href")?)?;
            let segments: Vec<_> = url
                .path_segments()?
                .filter(|part| !part.is_empty())
                .collect();
            let job_path = segments
                .windows(2)
                .any(|pair| matches!(pair[0], "job" | "jobs" | "careers") && !pair[1].is_empty());
            let lever = matches!(url.host_str(), Some("jobs.lever.co" | "jobs.eu.lever.co"))
                && segments.len() == 2;
            (job_path || lever).then(|| url.to_string())
        })
        .collect();
    (urls.len() == 1).then(|| urls.into_iter().next()).flatten()
}

pub(super) fn extract_alert_jobs_with_subject(html: &str, subject: &str) -> Vec<AlertJob> {
    let document = Html::parse_document(html);
    let mut jobs = Vec::new();
    for link in select(document.root_element(), "a[href]") {
        let Some((id, url)) = link.value().attr("href").and_then(canonical_linkedin_url) else {
            continue;
        };
        let card = card_for_link(link, &id);
        let anchor_title = select(link, "h1, h2, h3, [class*='job-title']")
            .into_iter()
            .map(text)
            .find(|title| !is_placeholder_title(title))
            .unwrap_or_else(|| text(link));
        let title = if !is_placeholder_title(&anchor_title) {
            anchor_title
        } else {
            select(card, "h1, h2, h3, [class*='job-title']")
                .into_iter()
                .map(text)
                .find(|title| !is_placeholder_title(title))
                .unwrap_or_else(|| "LinkedIn job alert".into())
        };
        let (company, location) = metadata(card, &title);
        jobs.push(AlertJob {
            id,
            url,
            title,
            company: company.unwrap_or_else(|| "Unknown company".into()),
            location,
            career_url: career_url(card),
        });
    }
    let mut jobs = merge_alert_jobs(jobs);
    // A subject can describe only the primary job, never every recommendation.
    if let Some((title, company)) = subject.split_once(" at ") {
        let title = title.trim().trim_end_matches(" job");
        let company = company
            .split_once(" - ")
            .filter(|(_, suffix)| {
                suffix.contains("school alumni")
                    || suffix.eq_ignore_ascii_case("Actively recruiting")
            })
            .map_or(company, |(company, _)| company)
            .trim();
        if !is_placeholder_title(title) && !company.is_empty() && !title.starts_with("You may be") {
            let unique_job = jobs.len() == 1;
            for job in &mut jobs {
                if normalized(&job.title) == normalized(title)
                    || (unique_job && is_placeholder_title(&job.title))
                {
                    if is_placeholder_title(&job.title) {
                        job.title = title.into();
                    }
                    if job.company == "Unknown company" {
                        job.company = company.into();
                    }
                }
            }
        }
    }
    jobs
}

#[cfg(test)]
pub(super) fn extract_alert_jobs(html: &str) -> Vec<AlertJob> {
    extract_alert_jobs_with_subject(html, "")
}

pub(super) fn merge_alert_jobs(jobs: Vec<AlertJob>) -> Vec<AlertJob> {
    let mut merged: Vec<AlertJob> = Vec::new();
    let mut positions = HashMap::new();
    for job in jobs {
        if let Some(&index) = positions.get(&job.id) {
            let previous: &mut AlertJob = &mut merged[index];
            if is_placeholder_title(&previous.title) && !is_placeholder_title(&job.title) {
                previous.title = job.title;
            }
            if previous.company == "Unknown company" && job.company != "Unknown company" {
                previous.company = job.company;
            }
            if previous.location.is_none() {
                previous.location = job.location;
            }
            if previous.career_url.is_none() {
                previous.career_url = job.career_url;
            }
        } else {
            positions.insert(job.id.clone(), merged.len());
            merged.push(job);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_cards_keep_metadata_and_company_links_separate() {
        let jobs = extract_alert_jobs_with_subject(
            include_str!("../fixture/linkedin_alert_email.html"),
            "Rust & Linux Engineer at Acme Systems",
        );
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].title, "Rust & Linux Engineer");
        assert_eq!(jobs[0].company, "Acme Systems");
        assert_eq!(jobs[0].location.as_deref(), Some("Taipei, Taiwan"));
        assert_eq!(
            jobs[0].career_url.as_deref(),
            Some("https://boards.greenhouse.io/acme/jobs/1234")
        );
        assert_eq!(jobs[1].title, "Senior C++ Engineer");
        assert_eq!(jobs[1].company, "Beta Devices");
        assert_eq!(jobs[1].location.as_deref(), Some("Hsinchu, Taiwan"));
        assert_eq!(
            jobs[1].career_url.as_deref(),
            Some("https://jobs.lever.co/beta/abc-123")
        );
        assert_eq!(jobs[2].company, "Unknown company");
        assert!(is_placeholder_title(&jobs[2].title));
        assert!(jobs[2].career_url.is_none());
    }

    #[test]
    fn later_email_can_fill_an_earlier_placeholder_without_losing_identity() {
        let mut jobs = extract_alert_jobs(
            "<a href='https://www.linkedin.com/jobs/view/4454432978/'>View job</a>",
        );
        jobs.extend(extract_alert_jobs(include_str!(
            "../fixture/linkedin_alert_email.html"
        )));
        let jobs = merge_alert_jobs(jobs);
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].id, "4454432978");
        assert_eq!(jobs[0].company, "Acme Systems");
        assert_eq!(jobs[0].title, "Rust & Linux Engineer");
    }

    #[test]
    fn subject_only_enriches_the_single_or_matching_primary_job() {
        let html = "<a href='https://www.linkedin.com/jobs/view/123/'>View job</a>";
        let jobs = extract_alert_jobs_with_subject(html, "Embedded Engineer job at Acme - Devices");
        assert_eq!(jobs[0].title, "Embedded Engineer");
        assert_eq!(jobs[0].company, "Acme - Devices");
        let multiple =
            format!("{html}<a href='https://www.linkedin.com/jobs/view/456/'>View job</a>");
        let jobs = extract_alert_jobs_with_subject(&multiple, "Embedded Engineer at Acme");
        assert!(jobs.iter().all(|job| job.company == "Unknown company"));
    }

    #[test]
    fn regional_comm_and_slug_urls_require_a_real_linkedin_host() {
        for url in [
            "https://tw.linkedin.com/jobs/view/rust-engineer-123/?trk=x",
            "https://www.linkedin.com/comm/jobs/view/123/?trk=x",
        ] {
            assert_eq!(canonical_linkedin_url(url).unwrap().0, "123");
        }
        assert!(
            canonical_linkedin_url(
                "https://evil.test/?url=https://www.linkedin.com/jobs/view/123/"
            )
            .is_none()
        );
        assert!(
            canonical_linkedin_url("https://www.linkedin.com.evil.test/jobs/view/123/").is_none()
        );
    }

    #[test]
    fn multiple_career_candidates_are_not_guessed() {
        let jobs = extract_alert_jobs(
            "<div><a href='https://www.linkedin.com/jobs/view/123/'>Engineer</a><p>Acme · Taipei, Taiwan</p><a href='https://jobs.lever.co/acme/first'>Apply</a><a href='https://jobs.lever.co/acme/second'>Apply</a></div>",
        );
        assert!(jobs[0].career_url.is_none());
    }

    #[test]
    fn nested_heading_and_br_metadata_do_not_become_a_combined_title() {
        let jobs = extract_alert_jobs(
            "<div><a href='https://www.linkedin.com/jobs/view/123/'><h3>Rust Engineer</h3></a><p>Acme <strong>Systems</strong><br>Taipei, Taiwan</p></div>",
        );
        assert_eq!(jobs[0].title, "Rust Engineer");
        assert_eq!(jobs[0].company, "Acme Systems");
        assert_eq!(jobs[0].location.as_deref(), Some("Taipei, Taiwan"));
        let nested = extract_alert_jobs(
            "<a href='https://www.linkedin.com/jobs/view/123/'><h3>Rust Engineer</h3><p>Acme Systems · Taipei, Taiwan</p></a>",
        );
        assert_eq!(nested[0].title, "Rust Engineer");
        assert_eq!(nested[0].company, "Acme Systems");
    }

    #[test]
    fn country_names_in_company_names_do_not_replace_the_location() {
        let jobs = extract_alert_jobs(
            "<div><a href='https://www.linkedin.com/jobs/view/123/'>Rust Engineer</a><p>Taiwan Semiconductor Manufacturing Company</p><p>Taipei, Taiwan</p></div>",
        );
        assert_eq!(
            jobs[0].company,
            "Taiwan Semiconductor Manufacturing Company"
        );
        assert_eq!(jobs[0].location.as_deref(), Some("Taipei, Taiwan"));
    }
}
