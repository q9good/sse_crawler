use anyhow::{anyhow, Context, Error};
use reqwest::{header, Client, Url};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::macros::date;
use time::{format_description, Date, PrimitiveDateTime};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

static DECLARE_SUBFOLDER: &str = "申报稿";
static REGISTER_SUBFOLDER: &str = "注册稿";
static MEETING_SUBFOLDER: &str = "上会稿";
static SPONSOR_SUBFOLDER: &str = "问询与回复/发行人与保荐机构";
static ACCOUNTANT_SUBFOLDER: &str = "问询与回复/会计师";
static LAWYER_SUBFOLDER: &str = "问询与回复/律师";
static RESULT_SUBFOLDER: &str = "结果";

static SUBFOLDERS: [&str; 7] = [
    DECLARE_SUBFOLDER,
    REGISTER_SUBFOLDER,
    MEETING_SUBFOLDER,
    SPONSOR_SUBFOLDER,
    ACCOUNTANT_SUBFOLDER,
    LAWYER_SUBFOLDER,
    RESULT_SUBFOLDER,
];

/// IPO result
#[repr(u8)]
#[derive(Debug)]
pub enum RegisterResult {
    // 1 - 注册生效
    RegisterEffective(Date),
    // 3 - 终止注册
    RegisterTerminated(Date),
}

/// audit status of IPO
#[repr(u8)]
#[derive(Debug)]
pub enum AuditStatus {
    // 1 - 已受理
    Accepted(Date),
    // 2 - 已问询
    Queried(Date),
    // 3 - 上市委会议
    Discussed(Date),
    // 4 - 提交注册
    Submitted(Date),
    // 5 - 注册生效 or 终止注册
    Registered(RegisterResult),
    // other
    // Todo
    Unsupported(u64, u64),
    Unknown,
}

/// the information about company which want to IPO in KCB
#[derive(Debug)]
pub struct CompanyInfo {
    // the company name
    stock_audit_name: String,
    // the audit id assigned to the company by see
    stock_audit_number: u32,
    // current status
    curr_status: AuditStatus,
    // the date submitting application
    apply_date: PrimitiveDateTime,
    // the date update information
    update_date: PrimitiveDateTime,
}

fn parse_date_from_sse(input: &str) -> anyhow::Result<PrimitiveDateTime> {
    let format = format_description::parse("[year][month][day][hour][minute][second]")?;
    let ret = PrimitiveDateTime::parse(input, &format)?;
    Ok(ret)
}

impl TryFrom<String> for CompanyInfo {
    type Error = anyhow::Error;
    fn try_from(resp: String) -> Result<Self, Self::Error> {
        #[allow(clippy::useless_format)]
        let json_str = format!(
            r#"{}"#,
            resp.split_terminator(&['(', ')'][..])
                .next_back()
                .context("invalid input")?
        );
        let json_body: Value = serde_json::from_str(&json_str)?;
        if matches!(&json_body["result"], Value::Array(result) if result.is_empty()) {
            return Err(anyhow!("empty company info"));
        }
        Ok(CompanyInfo {
            stock_audit_name: {
                // let company_name = json_body["result"][0]["stockAuditName"].as_str();
                let company_name = json_body["result"][0]["stockIssuer"][0]["s_issueCompanyFullName"].as_str();
                if let Some(temp) = company_name {
                    temp.trim().to_owned()
                } else {
                    return Err(anyhow!("get company name failed"));
                }
            },
            stock_audit_number: {
                let number = json_body["result"][0]["stockAuditNum"].as_str().unwrap();
                number.parse::<u32>().unwrap()
            },
            curr_status: {
                let status = json_body["result"][0]["currStatus"].as_u64();
                let result = json_body["result"][0]["registeResult"].as_u64();
                let update_date = json_body["result"][0]["updateDate"]
                    .as_str()
                    .context("acquire update time failed")?;
                let date = parse_date_from_sse(update_date)?;
                match (status, result) {
                    (Some(5), Some(1)) => {
                        AuditStatus::Registered(RegisterResult::RegisterEffective(date.date()))
                    }
                    (Some(5), Some(3)) => {
                        AuditStatus::Registered(RegisterResult::RegisterTerminated(date.date()))
                    }
                    (Some(4), _) => AuditStatus::Submitted(date.date()),
                    (Some(3), _) => AuditStatus::Discussed(date.date()),
                    (Some(2), _) => AuditStatus::Queried(date.date()),
                    (Some(1), _) => AuditStatus::Accepted(date.date()),
                    (Some(s), Some(r)) => AuditStatus::Unsupported(s, r),
                    (_, _) => AuditStatus::Unknown,
                }
            },
            apply_date: {
                let apply_date = json_body["result"][0]["auditApplyDate"]
                    .as_str()
                    .context("acquire apply_date failed")?;
                parse_date_from_sse(apply_date)?
            },
            update_date: {
                let update_date = json_body["result"][0]["updateDate"]
                    .as_str()
                    .context("acquire update_date failed")?;
                parse_date_from_sse(update_date)?
            },
        })
    }
}

#[derive(Debug)]
pub struct UploadFile {
    filename: String,
    url: Url,
    path: PathBuf,
}

#[derive(Debug)]
pub enum QueryReply {
    // 发行人,保荐机构
    Sponsor(UploadFile),
    // 会计师
    Accountant(UploadFile),
    // 律师
    Lawyer(UploadFile),
    // other
    Other(UploadFile),
}

/// 信息披露 & 问询与回复 & 注册结果文件
#[derive(Debug, Default)]
pub struct InfoDisclosure {
    /* #### 信息披露
     * ----
     * +. 1st element: 申报稿
     * +. 2nd element: 上会稿
     * +. 3rd element: 注册稿
     *
     */
    // 招股说明书
    prospectuses: [Option<UploadFile>; 3],
    // 发行保荐书
    publish_sponsor: [Option<UploadFile>; 3],
    // 上市保荐书
    list_sponsor: [Option<UploadFile>; 3],
    // 审计报告
    audit_report: [Option<UploadFile>; 3],
    // 法律意见书
    legal_opinion: [Option<UploadFile>; 3],
    // 其他
    others: [Option<UploadFile>; 3],
    /* #### 问询与回复
     * ----
     */
    query_and_reply: Vec<Option<QueryReply>>,
    /* #### 注册结果文件 and 终止审核通知
     * ----
     */
    register_result_or_audit_terminated: Vec<Option<UploadFile>>,
}

impl TryFrom<String> for InfoDisclosure {
    type Error = anyhow::Error;
    fn try_from(resp: String) -> Result<Self, Self::Error> {
        let pure_content: Vec<_> = resp.split_inclusive(&['(', ')'][..]).collect();
        #[allow(clippy::useless_format)]
        let mut json_str = format!(r#"{}"#, pure_content[1..].join(""));
        json_str.truncate(json_str.len() - 1);
        let json_body: Value = serde_json::from_str(&json_str)?;
        let mut infos = InfoDisclosure::default();
        let file_arr = json_body["result"]
            .as_array()
            .context("extract file array failed")?;
        let mut download_base = Url::parse("http://static.sse.com.cn/stock/")?;
        let ret = file_arr.iter().try_for_each(|x| {
            let mut file = UploadFile {
                filename: {
                    let name = x["fileTitle"].as_str().context("get filename failed")?;
                    name.to_owned()
                },
                url: {
                    let download_url = x["filePath"].as_str().context("get file url failed")?;
                    download_base.set_path(&*("stock".to_owned() + download_url));
                    download_base.to_owned()
                },
                path: {
                    let mut path = PathBuf::new();
                    path.push("Download");
                    path.push(
                        x["companyFullName"]
                            .as_str()
                            .context("get company name failed")?
                            .trim(),
                    );
                    path
                },
            };
            let file_type = x["fileType"].as_u64();
            let file_ver = x["fileVersion"].as_u64();
            match (file_type, file_ver) {
                // 招股说明书, 申报稿
                (Some(30), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[0] = Some(file);
                    Ok(())
                }
                // 招股说明书, 上会稿
                (Some(30), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[1] = Some(file);
                    Ok(())
                }
                // 招股说明书, 注册稿
                (Some(30), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[2] = Some(file);
                    Ok(())
                }
                // 发行保荐书, 申报稿
                (Some(36), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[0] = Some(file);
                    Ok(())
                }
                // 发行保荐书, 上会稿
                (Some(36), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[1] = Some(file);
                    Ok(())
                }
                // 发行保荐书, 注册稿
                (Some(36), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[2] = Some(file);
                    Ok(())
                }
                // 上市保荐书, 申报稿
                (Some(37), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[0] = Some(file);
                    Ok(())
                }
                // 上市保荐书, 上会稿
                (Some(37), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[1] = Some(file);
                    Ok(())
                }
                // 上市保荐书, 注册稿
                (Some(37), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[2] = Some(file);
                    Ok(())
                }
                // 审计报告, 申报稿
                (Some(32), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[0] = Some(file);
                    Ok(())
                }
                // 审计报告, 上会稿
                (Some(32), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[1] = Some(file);
                    Ok(())
                }
                // 审计报告, 注册稿
                (Some(32), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[2] = Some(file);
                    Ok(())
                }
                // 法律意见书, 申报稿
                (Some(33), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[0] = Some(file);
                    Ok(())
                }
                // 法律意见书, 上会稿
                (Some(33), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[1] = Some(file);
                    Ok(())
                }
                // 法律意见书, 注册稿
                (Some(33), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[2] = Some(file);
                    Ok(())
                }
                // 其他, 申报稿
                (Some(34), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[0] = Some(file);
                    Ok(())
                }
                // 其他, 上会稿
                (Some(34), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[1] = Some(file);
                    Ok(())
                }
                // 其他, 注册稿
                (Some(34), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[2] = Some(file);
                    Ok(())
                }
                // 问询和回复
                (Some(5) | Some(6), _) => {
                    // 发行人及保荐机构
                    if file.filename.starts_with("8-1") {
                        file.path.push(SPONSOR_SUBFOLDER);
                        file.path.push(&file.filename);
                        file.path.set_extension("pdf");
                        infos.query_and_reply.push(Some(QueryReply::Sponsor(file)));
                    } else if file.filename.starts_with("8-2") {
                        // 会计师
                        file.path.push(ACCOUNTANT_SUBFOLDER);
                        file.path.push(&file.filename);
                        file.path.set_extension("pdf");
                        infos
                            .query_and_reply
                            .push(Some(QueryReply::Accountant(file)));
                    } else if file.filename.starts_with("8-3") {
                        // 律师
                        file.path.push(LAWYER_SUBFOLDER);
                        file.path.push(&file.filename);
                        file.path.set_extension("pdf");
                        infos.query_and_reply.push(Some(QueryReply::Lawyer(file)));
                    } else {
                        file.path.push("问询与回复");
                        file.path.push(&file.filename);
                        file.path.set_extension("pdf");
                        infos.query_and_reply.push(Some(QueryReply::Other(file)));
                    }
                    Ok(())
                }
                // 注册结果通知和终止审核通知
                (Some(35) | Some(38), _) => {
                    file.path.push(RESULT_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.register_result_or_audit_terminated.push(Some(file));
                    Ok(())
                }
                // other
                (_, _) => Err(anyhow!("unknown file type")),
            }
        });
        if ret.is_ok() {
            Ok(infos)
        } else {
            Err(anyhow!("Error in parsing file path for InfoDisclosure"))
        }
    }
}

/// 上市委会议公告与结果
#[derive(Debug, Default)]
pub struct MeetingAnnounce {
    announcements: Vec<Option<UploadFile>>,
}

impl MeetingAnnounce {
    fn new(resp: String, id: u32) -> Result<MeetingAnnounce, anyhow::Error> {
        #[allow(clippy::useless_format)]
        let json_str = format!(
            r#"{}"#,
            resp.split_terminator(&['(', ')'][..])
                .next_back()
                .context("invalid input")?
        );
        let json_body: Value = serde_json::from_str(&json_str)?;
        let mut announce = MeetingAnnounce::default();
        let file_arr = json_body["result"]
            .as_array()
            .context("extract file array failed")?;
        let mut download_base = Url::parse("http://static.sse.com.cn/stock/")?;
        let ret: Result<(), anyhow::Error> = file_arr.iter().try_for_each(|x| {
            let file = UploadFile {
                filename: {
                    let name = x["fileTitle"].as_str().context("get filename failed")?;
                    name.to_owned()
                },
                url: {
                    let download_url = x["filePath"].as_str().context("get file url failed")?;
                    download_base.set_path(&*("stock".to_owned() + download_url));
                    download_base.to_owned()
                    // download_base.join(download_url)?
                },
                path: {
                    let stock_loop = x["stockAudit"].as_array().unwrap();
                    let company_name = {
                        let mut idx: usize = 0;
                        for i in 0..stock_loop.len() {
                            let audit_id = x["stockAudit"][i]["auditId"]
                                .as_str()
                                .unwrap()
                                .parse::<u32>()
                                .unwrap();
                            if audit_id == id {
                                idx = i;
                                break;
                            }
                        }
                        x["stockAudit"][idx]["companyFullName"]
                            .as_str()
                            .context("get company name failed")?
                            .trim()
                    };
                    // x["stockAudit"]
                    // [x["stockAudit"].as_array().unwrap().len() - 1]["companyFullName"]
                    // .as_str()
                    // .context("get company name failed")?;
                    let mut path = PathBuf::new();
                    path.push("Download");
                    path.push(company_name);
                    path.push(RESULT_SUBFOLDER);
                    path.push(x["fileTitle"].as_str().unwrap());
                    path.set_extension("pdf");
                    path
                },
            };
            announce.announcements.push(Some(file));
            Ok(())
        });
        if ret.is_ok() {
            Ok(announce)
        } else {
            Err(anyhow!("Error in parsing path for MeetingAnnounce"))
        }
    }
}

/// 公司信息汇总
#[derive(Debug)]
pub struct ItemDetail {
    overview: CompanyInfo,
    disclosure: InfoDisclosure,
    announce: MeetingAnnounce,
}

#[derive(Debug, Clone)]
pub struct ReqClient(Client);

impl ReqClient {
    pub fn new() -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert("User-Agent", header::HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.93 Safari/537.36"));
        headers.insert(
            "Referer",
            header::HeaderValue::from_static("https://kcb.sse.com.cn"),
        );
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()
            .unwrap();
        ReqClient(client)
    }
}

/// 爬虫入口
#[derive(Debug)]
pub struct SseQuery {
    // reqwest client
    // client: Client,
    // 所有公司信息
    pub companies: Vec<ItemDetail>,
    // 出错的公司名字，需人工处理
    pub failed_logs: Vec<String>,
}

impl SseQuery {
    pub fn new() -> Self {
        Self {
            companies: Vec::new(),
            failed_logs: Vec::new(),
        }
    }

    pub fn add(&mut self, company: std::result::Result<ItemDetail, String>) {
        match company {
            Ok(info) => self.companies.push(info),
            Err(name) => self.failed_logs.push(name),
        }
    }
}

async fn query_company_overview(client: &mut ReqClient, name: &str) -> Result<CompanyInfo, Error> {
    let url = format!("http://query.sse.com.cn/statusAction.do?jsonCallBack=jsonpCallback42305&isPagination=true&sqlId=SH_XM_LB&pageHelp.pageSize=20&offerType=&commitiResult=&registeResult=&province=&csrcCode=&currStatus=&order=&keyword={}&auditApplyDateBegin=&auditApplyDateEnd=&_=1640867539069", name);
    let resp = client.0.get(url).send().await?;

    let body = resp.text().await?;
    Ok(CompanyInfo::try_from(body)?)
}

async fn query_company_disclosure(
    client: &mut ReqClient,
    id: u32,
) -> Result<InfoDisclosure, Error> {
    let url = format!("http://query.sse.com.cn/commonSoaQuery.do?jsonCallBack=jsonpCallback99435173&isPagination=false&sqlId=GP_GPZCZ_SHXXPL&stockAuditNum={}&_=1641094982593", id);
    let resp = client.0.get(url).send().await?;

    let body = resp.text().await?;
    Ok(InfoDisclosure::try_from(body)?)
}

async fn query_company_announce(client: &mut ReqClient, id: u32) -> Result<MeetingAnnounce, Error> {
    let url = format!("http://query.sse.com.cn/commonSoaQuery.do?jsonCallBack=jsonpCallback42495292&isPagination=false&sqlId=GP_GPZCZ_SSWHYGGJG&fileType=1,2,3,4&stockAuditNum={}&_=1641114627446", id);
    let resp = client.0.get(url).send().await?;

    let body = resp.text().await?;
    Ok(MeetingAnnounce::new(body, id)?)
}

pub async fn process_company(
    client: &mut ReqClient,
    name: &str,
) -> std::result::Result<ItemDetail, String> {
    let mut audit_id: u32 = 0;
    let company_info = query_company_overview(client, name).await;
    if company_info.is_ok() {
        audit_id = company_info.as_ref().unwrap().stock_audit_number;
        let disclosure = query_company_disclosure(client, audit_id).await;
        let announce = query_company_announce(client, audit_id).await;
        if disclosure.is_ok() && announce.is_ok() {
            let item = ItemDetail {
                overview: company_info.unwrap(),
                disclosure: disclosure.unwrap(),
                announce: announce.unwrap(),
            };
            // #[cfg(not(test))]
            {
                let ret = download_company_files(client, &item).await;
                match ret {
                    Ok(_) => Ok(item),
                    Err(e) => {
                        let mut err_msg = format!("{}", e);
                        err_msg.push_str(name);
                        Err(err_msg)
                    }
                }
            }
            // #[cfg(test)]
            // Ok(item)
        } else {
            Err(name.to_owned())
        }
    } else {
        Err(name.to_owned())
    }
}

pub async fn download_company_files(
    client: &mut ReqClient,
    company: &ItemDetail,
) -> anyhow::Result<()> {
    let base_folder = &company.overview.stock_audit_name;
    // let client = ReqClient::new();

    // create SUBFOLDERS to save pdf files
    SUBFOLDERS.map(|folder| {
        let sub_folder: PathBuf = ["Download", base_folder, folder]
            .iter()
            .collect::<PathBuf>();
        std::fs::create_dir_all(sub_folder).unwrap_or_else(|why| println!("! {:?}", why.kind()));
    });

    let mut download_tasks = Vec::<(&Url, &PathBuf)>::new();
    company.disclosure.prospectuses.iter().for_each(|x| {
        if x.is_some() {
            let y = x.as_ref().unwrap();
            download_tasks.push((&y.url, &y.path));
        }
    });
    company.disclosure.publish_sponsor.iter().for_each(|x| {
        if x.is_some() {
            let y = x.as_ref().unwrap();
            download_tasks.push((&y.url, &y.path));
        }
    });
    company.disclosure.list_sponsor.iter().for_each(|x| {
        if x.is_some() {
            let y = x.as_ref().unwrap();
            download_tasks.push((&y.url, &y.path));
        }
    });
    company.disclosure.audit_report.iter().for_each(|x| {
        if x.is_some() {
            let y = x.as_ref().unwrap();
            download_tasks.push((&y.url, &y.path));
        }
    });
    company.disclosure.legal_opinion.iter().for_each(|x| {
        if x.is_some() {
            let y = x.as_ref().unwrap();
            download_tasks.push((&y.url, &y.path));
        }
    });
    company.disclosure.others.iter().for_each(|x| {
        if x.is_some() {
            let y = x.as_ref().unwrap();
            download_tasks.push((&y.url, &y.path));
        }
    });
    company.disclosure.query_and_reply.iter().for_each(|x| {
        let y = x.as_ref().unwrap();
        match y {
            QueryReply::Sponsor(z) => download_tasks.push((&z.url, &z.path)),
            QueryReply::Accountant(z) => download_tasks.push((&z.url, &z.path)),
            QueryReply::Lawyer(z) => download_tasks.push((&z.url, &z.path)),
            QueryReply::Other(z) => download_tasks.push((&z.url, &z.path)),
        }
    });
    company
        .disclosure
        .register_result_or_audit_terminated
        .iter()
        .for_each(|x| {
            let y = x.as_ref().unwrap();
            download_tasks.push((&y.url, &y.path));
        });
    company.announce.announcements.iter().for_each(|x| {
        let y = x.as_ref().unwrap();
        download_tasks.push((&y.url, &y.path));
    });
    #[cfg(test)]
    println!("{:#?}", download_tasks);
    for (url, path) in download_tasks {
        // println!("{:#?}", url.clone().as_str());
        if !path.exists() {
            let resp = client.0.get(url.clone()).send().await?;
            let content = resp.bytes().await?;
            let mut file = File::create(path).await?;
            file.write_all(&content).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Instant;

    #[tokio::test]
    async fn test_store_cookie_automatically() {
        let mut headers = header::HeaderMap::new();
        headers.insert("User-Agent", header::HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.93 Safari/537.36"));
        headers.insert(
            "Referer",
            header::HeaderValue::from_static("https://kcb.sse.com.cn"),
        );
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()
            .unwrap();
        let mut res = client
            .get("http://kcb.sse.com.cn/renewal")
            .send()
            .await
            .unwrap();
        res = client.get("http://query.sse.com.cn/commonSoaQuery.do?jsonCallBack=jsonpCallback42916568&isPagination=false&sqlId=GP_GPZCZ_SHXXPL&stockAuditNum=961&_=1640614222583")
            .send()
            .await
            .unwrap();

        let mut body = res.text().await.unwrap();
        // let body =res.json::<HashMap<String, String>>()
        // .await.unwrap();
        println!("{:?}", body);
        println!("{:?}", client);
    }

    #[tokio::test]
    async fn test_query_company_brief() {
        let mut headers = header::HeaderMap::new();
        headers.insert("User-Agent", header::HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.93 Safari/537.36"));
        headers.insert(
            "Referer",
            header::HeaderValue::from_static("https://kcb.sse.com.cn"),
        );
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()
            .unwrap();
        let mut res = client
            .get("http://kcb.sse.com.cn/renewal")
            .send()
            .await
            .unwrap();
        let url = format!("http://query.sse.com.cn/statusAction.do?jsonCallBack=jsonpCallback42305&isPagination=true&sqlId=SH_XM_LB&pageHelp.pageSize=20&offerType=&commitiResult=&registeResult=&province=&csrcCode=&currStatus=&order=&keyword={}&auditApplyDateBegin=&auditApplyDateEnd=&_=1640867539069","北京英诺特生物技术股份有限公司");
        res = client.get(url).send().await.unwrap();

        // let mut body = res.text().await.unwrap();
        let body = res.text().await.unwrap();

        let json_str = format!(
            r#"{}"#,
            body.split_terminator(&['(', ')'][..]).next_back().unwrap()
        );
        let json: Value = serde_json::from_str(&json_str).unwrap();
        println!("{:#?}", json);
        // println!("{:?}", client);
    }

    #[test]
    fn test_company_info_try_from_json() {
        let raw_data = String::from(
            r#"jsonpCallback85975({"actionErrors":[],"actionMessages":[],"downloadFileName":null,"execlStream":null,"fieldErrors":{},"fileId":null,"isPagination":"true","jsonCallBack":"jsonpCallback85975","locale":"zh_CN","mergeSize":null,"mergeType":null,"pageHelp":{"beginPage":1,"cacheSize":1,"data":[{"updateDate":"20211230191852","planIssueCapital":12.09,"suspendStatus":"","wenHao":"","stockAuditName":"北京英诺特生物技术股份有限公司","currStatus":2,"stockAuditNum":"924","registeResult":"","intermediary":[{"auditId":"924","i_intermediaryType":1,"i_intermediaryId":"0_1047","i_person":[{"i_p_personName":"董炜源","i_p_jobType":24,"i_p_personId":"787524","i_p_jobTitle":"项目协办人"},{"i_p_personName":"郑明欣","i_p_jobType":23,"i_p_personId":"787523","i_p_jobTitle":"保荐代表人B"},{"i_p_personName":"丁明明","i_p_jobType":22,"i_p_personId":"787522","i_p_jobTitle":"保荐代表人A"},{"i_p_personName":"唐松华","i_p_jobType":21,"i_p_personId":"787519","i_p_jobTitle":"保荐业务负责人"}],"i_intermediaryAbbrName":"华泰联合证券","i_intermediaryName":"华泰联合证券有限责任公司","i_intermediaryOrder":1},{"auditId":"924","i_intermediaryType":2,"i_intermediaryId":"20008","i_person":[{"i_p_personName":"胡咏华","i_p_jobType":31,"i_p_personId":"787537","i_p_jobTitle":"会计事务所负责人"},{"i_p_personName":"丁亭亭","i_p_jobType":34,"i_p_personId":"787540","i_p_jobTitle":"签字会计师C（或有）"},{"i_p_personName":"岑溯鹏","i_p_jobType":33,"i_p_personId":"787539","i_p_jobTitle":"签字会计师B"},{"i_p_personName":"牛良文","i_p_jobType":32,"i_p_personId":"787538","i_p_jobTitle":"签字会计师A"}],"i_intermediaryAbbrName":"大信","i_intermediaryName":"大信会计师事务所（特殊普通合伙）","i_intermediaryOrder":1},{"auditId":"924","i_intermediaryType":3,"i_intermediaryId":"10006","i_person":[{"i_p_personName":"魏海涛","i_p_jobType":42,"i_p_personId":"787543","i_p_jobTitle":"签字律师A"},{"i_p_personName":"姚启明","i_p_jobType":43,"i_p_personId":"787544","i_p_jobTitle":"签字律师B"},{"i_p_personName":"张学兵","i_p_jobType":41,"i_p_personId":"787542","i_p_jobTitle":"律师事务所负责人"},{"i_p_personName":"丁文昊","i_p_jobType":44,"i_p_personId":"787545","i_p_jobTitle":"签字律师C（或有）"}],"i_intermediaryAbbrName":"北京市中伦","i_intermediaryName":"北京市中伦律师事务所","i_intermediaryOrder":1}],"collectType":1,"stockIssuer":[{"s_personName":"叶逢光","auditId":"924","s_stockIssueId":"924","s_personId":"787515","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"董事长","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"张秀杰","auditId":"924","s_stockIssueId":"924","s_personId":"787516","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"总经理","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"陈富康","auditId":"924","s_stockIssueId":"924","s_personId":"787517","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"董秘","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"","auditId":"924","s_stockIssueId":"924","s_personId":"787518","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"其他","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""}],"createTime":"20210607144024","auditApplyDate":"20210616165743","issueAmount":"","commitiResult":"","issueMarketType":1,"OPERATION_SEQ":"17091ec3adcced13ef280fdbf3c35881"}],"endDate":null,"endPage":null,"objectResult":null,"pageCount":1,"pageNo":1,"pageSize":20,"searchDate":null,"sort":null,"startDate":null,"total":1},"pageNo":null,"pageSize":null,"queryDate":"","result":[{"updateDate":"20211230191852","planIssueCapital":12.09,"suspendStatus":"","wenHao":"","stockAuditName":"北京英诺特生物技术股份有限公司","currStatus":2,"stockAuditNum":"924","registeResult":"","intermediary":[{"auditId":"924","i_intermediaryType":1,"i_intermediaryId":"0_1047","i_person":[{"i_p_personName":"董炜源","i_p_jobType":24,"i_p_personId":"787524","i_p_jobTitle":"项目协办人"},{"i_p_personName":"郑明欣","i_p_jobType":23,"i_p_personId":"787523","i_p_jobTitle":"保荐代表人B"},{"i_p_personName":"丁明明","i_p_jobType":22,"i_p_personId":"787522","i_p_jobTitle":"保荐代表人A"},{"i_p_personName":"唐松华","i_p_jobType":21,"i_p_personId":"787519","i_p_jobTitle":"保荐业务负责人"}],"i_intermediaryAbbrName":"华泰联合证券","i_intermediaryName":"华泰联合证券有限责任公司","i_intermediaryOrder":1},{"auditId":"924","i_intermediaryType":2,"i_intermediaryId":"20008","i_person":[{"i_p_personName":"胡咏华","i_p_jobType":31,"i_p_personId":"787537","i_p_jobTitle":"会计事务所负责人"},{"i_p_personName":"丁亭亭","i_p_jobType":34,"i_p_personId":"787540","i_p_jobTitle":"签字会计师C（或有）"},{"i_p_personName":"岑溯鹏","i_p_jobType":33,"i_p_personId":"787539","i_p_jobTitle":"签字会计师B"},{"i_p_personName":"牛良文","i_p_jobType":32,"i_p_personId":"787538","i_p_jobTitle":"签字会计师A"}],"i_intermediaryAbbrName":"大信","i_intermediaryName":"大信会计师事务所（特殊普通合伙）","i_intermediaryOrder":1},{"auditId":"924","i_intermediaryType":3,"i_intermediaryId":"10006","i_person":[{"i_p_personName":"魏海涛","i_p_jobType":42,"i_p_personId":"787543","i_p_jobTitle":"签字律师A"},{"i_p_personName":"姚启明","i_p_jobType":43,"i_p_personId":"787544","i_p_jobTitle":"签字律师B"},{"i_p_personName":"张学兵","i_p_jobType":41,"i_p_personId":"787542","i_p_jobTitle":"律师事务所负责人"},{"i_p_personName":"丁文昊","i_p_jobType":44,"i_p_personId":"787545","i_p_jobTitle":"签字律师C（或有）"}],"i_intermediaryAbbrName":"北京市中伦","i_intermediaryName":"北京市中伦律师事务所","i_intermediaryOrder":1}],"collectType":1,"stockIssuer":[{"s_personName":"叶逢光","auditId":"924","s_stockIssueId":"924","s_personId":"787515","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"董事长","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"张秀杰","auditId":"924","s_stockIssueId":"924","s_personId":"787516","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"总经理","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"陈富康","auditId":"924","s_stockIssueId":"924","s_personId":"787517","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"董秘","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"","auditId":"924","s_stockIssueId":"924","s_personId":"787518","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"其他","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""}],"createTime":"20210607144024","auditApplyDate":"20210616165743","issueAmount":"","commitiResult":"","issueMarketType":1,"OPERATION_SEQ":"17091ec3adcced13ef280fdbf3c35881"}],"securityCode":"","statistics":[{"num":31,"status":"1"},{"num":61,"status":"2"},{"num":405,"status":"5"},{"num":48,"status":"4"},{"num":145,"status":"8"},{"num":7,"status":"7"},{"num":12,"status":"3"},{"num":2,"status":"3-6"},{"num":10,"status":"3-1"},{"num":1,"status":"5-2"},{"num":390,"status":"5-1"},{"num":14,"status":"5-3"},{"num":6,"status":"7-1"},{"num":1,"status":"7-2"}],"texts":null,"type":"","validateCode":""})"#,
        );
        let company_info = CompanyInfo::try_from(raw_data);
        println!("{:#?}", company_info);
    }

    #[test]
    fn test_invalid_company_info_try_from_json() {
        let raw_data = String::from(
            r#"jsonpCallback70586({"actionErrors":[],"actionMessages":[],"downloadFileName":null,"execlStream":null,"fieldErrors":{},"fileId":null,"isPagination":"true","jsonCallBack":"jsonpCallback70586","locale":"zh_CN","mergeSize":null,"mergeType":null,"pageHelp":{"beginPage":0,"cacheSize":1,"data":[],"endDate":null,"endPage":null,"objectResult":null,"pageCount":0,"pageNo":1,"pageSize":20,"searchDate":null,"sort":null,"startDate":null,"total":0},"pageNo":null,"pageSize":null,"queryDate":"","result":[],"securityCode":"","statistics":[{"num":31,"status":"1"},{"num":61,"status":"2"},{"num":405,"status":"5"},{"num":48,"status":"4"},{"num":145,"status":"8"},{"num":7,"status":"7"},{"num":12,"status":"3"},{"num":2,"status":"3-6"},{"num":10,"status":"3-1"},{"num":1,"status":"5-2"},{"num":390,"status":"5-1"},{"num":14,"status":"5-3"},{"num":6,"status":"7-1"},{"num":1,"status":"7-2"}],"texts":null,"type":"","validateCode":""})"#,
        );
        let company_info = CompanyInfo::try_from(raw_data);
        println!("{:#?}", company_info);
    }

    #[test]
    fn test_info_disclosure_try_from_json() {
        let raw_data = String::from(
            r#"jsonpCallback99435173({"actionErrors":[],"actionMessages":[],"fieldErrors":{},"isPagination":"false","jsonCallBack":"jsonpCallback99435173","locale":"en_US","pageHelp":{"beginPage":1,"cacheSize":1,"data":[{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/d078b75188094a5d9848073e3099dc97.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"506c1fe4277c4b39aa750b4d3a8fa22b","filename":"d078b75188094a5d9848073e3099dc97.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":638900,"StockType":1,"fileTitle":"8-3 补充法律意见书","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021742","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/6550a785ae9f4c01bcad74d8ed07e339.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"ab6fe7cdbc194ff985b3f3d7d687f1c5","filename":"6550a785ae9f4c01bcad74d8ed07e339.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":3188045,"StockType":1,"fileTitle":"8-2 会计师关于德邦科技科创板上市首轮问询回复意见","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021741","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/99858ffdaa4a45c18028374b840b135b.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"3df19f054fe749ecb5ddd1372b26884e","filename":"99858ffdaa4a45c18028374b840b135b.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":3114905,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于审核问询函的回复(豁免版)","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021740","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/dbdbfa8875ef40298638549ab1524817.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"dbdbfa8875ef40298638549ab1524817.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":11888035,"StockType":1,"fileTitle":"永拓会计师事务所（特殊普通合伙）关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":1,"fileVersion":1,"fileType":32,"MarketType":1,"fileId":"209049","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/e5b671d8cec14bf3883e8abd45fc3a96.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"e5b671d8cec14bf3883e8abd45fc3a96.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":736749,"StockType":1,"fileTitle":"东方证券承销保荐有限公司关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":1,"fileVersion":1,"fileType":37,"MarketType":1,"fileId":"209043","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/666b8b084d1e4720bc90aa19a40fab5a.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"666b8b084d1e4720bc90aa19a40fab5a.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":703086,"StockType":1,"fileTitle":"东方证券承销保荐有限公司关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":1,"fileVersion":1,"fileType":36,"MarketType":1,"fileId":"209041","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"}],"endDate":null,"endPage":null,"objectResult":null,"pageCount":1,"pageNo":1,"pageSize":6,"searchDate":null,"sort":null,"startDate":null,"total":6},"pageNo":null,"pageSize":null,"queryDate":"","result":[{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/d078b75188094a5d9848073e3099dc97.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"506c1fe4277c4b39aa750b4d3a8fa22b","filename":"d078b75188094a5d9848073e3099dc97.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":638900,"StockType":1,"fileTitle":"8-3 补充法律意见书","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021742","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/6550a785ae9f4c01bcad74d8ed07e339.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"ab6fe7cdbc194ff985b3f3d7d687f1c5","filename":"6550a785ae9f4c01bcad74d8ed07e339.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":3188045,"StockType":1,"fileTitle":"8-2 会计师关于德邦科技科创板上市首轮问询回复意见","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021741","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/99858ffdaa4a45c18028374b840b135b.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"3df19f054fe749ecb5ddd1372b26884e","filename":"99858ffdaa4a45c18028374b840b135b.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":3114905,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于审核问询函的回复(豁免版)","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021740","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/dbdbfa8875ef40298638549ab1524817.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"dbdbfa8875ef40298638549ab1524817.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":11888035,"StockType":1,"fileTitle":"永拓会计师事务所（特殊普通合伙）关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":1,"fileVersion":1,"fileType":32,"MarketType":1,"fileId":"209049","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/e5b671d8cec14bf3883e8abd45fc3a96.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"e5b671d8cec14bf3883e8abd45fc3a96.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":736749,"StockType":1,"fileTitle":"东方证券承销保荐有限公司关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":1,"fileVersion":1,"fileType":37,"MarketType":1,"fileId":"209043","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/666b8b084d1e4720bc90aa19a40fab5a.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"666b8b084d1e4720bc90aa19a40fab5a.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":703086,"StockType":1,"fileTitle":"东方证券承销保荐有限公司关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":1,"fileVersion":1,"fileType":36,"MarketType":1,"fileId":"209041","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"}],"securityCode":"","texts":null,"type":"","validateCode":""})"#,
        );
        let disclosure = InfoDisclosure::try_from(raw_data);
        println!("{:#?}", disclosure);
    }

    #[tokio::test]
    async fn test_query_company_info() {
        let mut client = ReqClient::new();
        let company = query_company_overview(&mut client, "大汉软件股份有限公司")
            .await
            .unwrap();
        println!("{:#?}", company)
    }

    #[tokio::test]
    async fn test_query_company_disclosure() {
        let mut client = ReqClient::new();
        let info = query_company_disclosure(&mut client, 759).await.unwrap();
        println!("{:#?}", info)
    }

    #[tokio::test]
    async fn test_query_company_announce() {
        let mut client = ReqClient::new();
        let announce = query_company_announce(&mut client, 759).await.unwrap();
        println!("{:#?}", announce)
    }

    #[tokio::test]
    async fn test_process_company() {
        let mut client = ReqClient::new();
        let sse = process_company(&mut client, "广东安达智能装备股份有限公司").await;
        println!("{:#?}", sse);
    }

    #[tokio::test]
    async fn test_process_more_companies() {
        // let mut sse = Arc::new(Mutex::new(SseCrawler::new()));
        let now = Instant::now();
        let mut client = ReqClient::new();
        let mut sse = SseQuery::new();
        let companies = [
            "上海赛伦生物技术股份有限公司",
            "大汉软件股份有限公司",
            "浙江海正生物材料股份有限公司",
            "江苏集萃药康生物科技股份有限公司",
        ];
        for i in 0..companies.len() {
            let info = process_company(&mut client, companies[i]).await;
            download_company_files(&mut client, &info.as_ref().unwrap())
                .await
                .unwrap();
            sse.add(info);
        }
        println!("{:#?}", sse);
        println!("总耗时：{} ms", now.elapsed().as_millis());
    }

    #[tokio::test]
    async fn test_process_more_companies_true_async() {
        let now = Instant::now();
        let mut sse = Arc::new(Mutex::new(SseQuery::new()));
        let companies = [
            "上海赛伦生物技术股份有限公司",
            "大汉软件股份有限公司",
            "浙江海正生物材料股份有限公司",
            "江苏集萃药康生物科技股份有限公司",
        ];
        let mut handles = Vec::with_capacity(companies.len());
        for i in 0..companies.len() {
            let sse_copy = sse.clone();
            handles.push(tokio::spawn(async move {
                let mut client = ReqClient::new();
                let ret = process_company(&mut client, companies[i]).await;
                download_company_files(&mut client, &ret.as_ref().unwrap()).await;
                let mut copy = sse_copy.lock().await;
                copy.add(ret);
            }));
        }
        for handle in handles {
            handle.await;
        }

        // sleep(Duration::from_secs(20)).await;
        println!("{:#?}", sse);
        println!("总耗时：{} ms", now.elapsed().as_millis());
    }

    #[tokio::test]
    async fn test_create_subfolder() {
        let mut sse = SseQuery::new();
        let mut client = ReqClient::new();
        let item = process_company(&mut client, "大汉软件股份有限公司").await;
        sse.add(item);
        download_company_files(&mut client, &sse.companies[0]).await;
        // println!("{:#?}", sse);
    }

    #[tokio::test]
    async fn test_control_concurrency_num() {
        let now = Instant::now();
        let mut sse = Arc::new(Mutex::new(SseQuery::new()));
        let companies = [
            "上海赛伦生物技术股份有限公司",
            "大汉软件股份有限公司",
            "浙江海正生物材料股份有限公司",
            "江苏集萃药康生物科技股份有限公司",
        ];
        let idx: Vec<usize> = (0..companies.len()).collect();
        for chunk in idx.chunks(2) {
            let mut handles = Vec::with_capacity(2);
            for &elem in chunk.iter() {
                let sse_copy = sse.clone();
                handles.push(tokio::spawn(async move {
                    let mut client = ReqClient::new();
                    let ret = process_company(&mut client, companies[elem]).await;
                    download_company_files(&mut client, &ret.as_ref().unwrap()).await;
                    let mut copy = sse_copy.lock().await;
                    copy.add(ret);
                }));
            }
            for handle in handles {
                handle.await;
            }
            println!("耗时：{} ms", now.elapsed().as_millis());
        }

        // sleep(Duration::from_secs(20)).await;
        // println!("{:#?}", sse);
        println!("总耗时：{} ms", now.elapsed().as_millis());
    }
}
