use crate::sse::sse::{process_company, ReqClient, SseQuery};
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Mutex;

mod sse;
static MAX_CONCURRENCY: usize = 4;



static SSE_COMPANIES: &str = "常州银河世纪微电子股份有限公司,
株洲欧科亿数控精密刀具股份有限公司,
深圳市明微电子股份有限公司,
上海健耕医药科技股份有限公司,
北京青云科技股份有限公司,
杭华油墨股份有限公司,
株洲华锐精密工具股份有限公司,
芯海科技（深圳）股份有限公司,
深圳市迅捷兴科技股份有限公司,
大连豪森设备制造股份有限公司,
浙江海盐力源环保科技股份有限公司,
杭州西力智能科技股份有限公司,
精英数智科技股份有限公司,
泰州亿腾景昂药业股份有限公司,
山东科汇电力自动化股份有限公司,
江苏诺泰澳赛诺生物制药股份有限公司,
昆山东威科技股份有限公司,
深圳新益昌科技股份有限公司,
杭州柯林电气股份有限公司,
深圳市科思科技股份有限公司,
国网智能科技股份有限公司,
深圳市紫光照明技术股份有限公司,
上海海优威新材料股份有限公司,
成都极米科技股份有限公司,
九号有限公司,
深圳市正弦电气股份有限公司,
南通星球石墨股份有限公司,
北京诺禾致源科技股份有限公司,
深圳市亚辉龙生物科技股份有限公司,
北京凯因科技股份有限公司,
广州三孚新材料科技股份有限公司,
成都圣诺生物科技股份有限公司,
宁波天益医疗器械股份有限公司,
广东博力威科技股份有限公司,
慧翰微电子股份有限公司,
苏州纳微科技股份有限公司,
深圳惠泰医疗器械股份有限公司,
广东九联科技股份有限公司,
成都欧林生物科技股份有限公司,
成都纵横自动化技术股份有限公司,
北京芯愿景软件技术股份有限公司,
奥精医疗科技股份有限公司,
赛赫智能设备（上海）股份有限公司,
桂林智神信息技术股份有限公司,
上海电气风电集团股份有限公司,
东软医疗系统股份有限公司,
罗普特科技集团股份有限公司,
江苏浩欧博生物医药股份有限公司,
杭州美迪凯光电科技股份有限公司,
江苏康众数字医疗科技股份有限公司,
上海和辉光电股份有限公司,
安徽省通源环境节能股份有限公司,
东来涂料技术（上海）股份有限公司,
生益电子股份有限公司,
天臣国际医疗科技股份有限公司,
广东莱尔新材料科技股份有限公司,
江西金达莱环保股份有限公司,
上海霍莱沃电子系统技术股份有限公司,
广东鸿铭智能股份有限公司,
新风光电子科技股份有限公司,
无锡力芯微电子股份有限公司,
杭州品茗安控信息技术股份有限公司,
上海睿昂基因科技股份有限公司,
会通新材料股份有限公司,
呈和科技股份有限公司,
优利德科技（中国）股份有限公司,
上海合晶硅材料股份有限公司,
成都智明达电子股份有限公司,
苏州艾隆科技股份有限公司,
上海宏力达信息技术股份有限公司,
苏州明志科技股份有限公司,
上海新致软件股份有限公司,
科美诊断技术股份有限公司,
深圳微众信用科技股份有限公司,
上海皓元医药股份有限公司,
浙江蓝特光学股份有限公司,
江苏联测机电科技股份有限公司,
杭州奥泰生物技术股份有限公司,
青岛中加特电气股份有限公司,
江苏富淼科技股份有限公司,
福建福昕软件开发股份有限公司,
西安康拓医疗技术股份有限公司,
蚂蚁科技集团股份有限公司";

#[tokio::main]
async fn main() {
    let companies: Vec<_> = SSE_COMPANIES
        .split_terminator(',')
        .map(|x| x.trim())
        .collect();

    let mut sse = Arc::new(Mutex::new(SseQuery::new()));
    let idx: Vec<usize> = (0..companies.len()).collect();
    let companies_ptr = companies.as_ptr();
    for chunk in idx.chunks(MAX_CONCURRENCY) {
        let mut handles = Vec::with_capacity(MAX_CONCURRENCY);
        for &elem in chunk.iter() {
            let sse_copy = sse.clone();
            let company_copy = companies[elem];
            handles.push(tokio::spawn(async move {
                println!("processing {}", company_copy);
                let mut client = ReqClient::new();
                let ret = process_company(&mut client, company_copy).await;
                let mut copy = sse_copy.lock().await;
                copy.add(ret);
            }));
        }
        for handle in handles {
            handle.await;
        }
        // std::thread::sleep(std::time::Duration::from_secs(10));
    }
    let path = std::path::PathBuf::from(r"failed_logs.txt");
    let mut file;
    if !path.exists() {
        file = std::fs::File::create(path).unwrap();
    } else {
        file = std::fs::File::open(path).unwrap();
    }
    let sse_result = sse.lock().await;
    let content = sse_result.failed_logs.join("\n");
    file.write_all(content.as_ref());
}
