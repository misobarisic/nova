/* Included after packet-copy helpers: this path never opens a destination file. */
typedef int (*Emit)(void *, int, const uint8_t *, int, double, double, double);
typedef struct { uint8_t *bytes; int64_t size,capacity,limit; Emit emit;void *opaque;Job *job;double origin;int initialized; } StreamBuffer;
static int stream_write(void *opaque,NOVA_WRITE_CONST uint8_t *data,int size){
    StreamBuffer *b=opaque;if(interrupted(b->job))return AVERROR_EXIT;
    if(size<0||size>b->limit-b->size)return AVERROR(ENOSPC);
    int64_t needed=b->size+size;
    if(needed>b->capacity){
        int64_t capacity=b->capacity?b->capacity:FFMIN(32768,b->limit);
        while(capacity<needed&&capacity<b->limit)capacity=FFMIN(capacity*2,b->limit);
        uint8_t *bytes=av_realloc(b->bytes,capacity);if(!bytes)return AVERROR(ENOMEM);
        b->bytes=bytes;b->capacity=capacity;
    }
    memcpy(b->bytes+b->size,data,size);b->size=needed;return size;
}
static int stream_emit(StreamBuffer *b,int event,double start,double duration){
    if(interrupted(b->job))return AVERROR_EXIT;
    if(event==1&&!b->initialized){
        int64_t at=0;
        while(at+8<=b->size){
            uint32_t size=((uint32_t)b->bytes[at]<<24)|((uint32_t)b->bytes[at+1]<<16)|((uint32_t)b->bytes[at+2]<<8)|b->bytes[at+3];
            if(size<8||size>b->size-at)return AVERROR_INVALIDDATA;
            if(!memcmp(b->bytes+at+4,"moof",4))break;
            at+=size;
        }
        if(at>1048576)return AVERROR(ENOSPC);
        if(at==b->size)return AVERROR_INVALIDDATA;
        int ret=b->emit(b->opaque,0,b->bytes,(int)at,b->origin,0,0);if(ret<0)return ret;
        memmove(b->bytes,b->bytes+at,b->size-at);b->size-=at;b->initialized=1;
    }
    int ret=b->emit(b->opaque,event,b->bytes,(int)b->size,b->origin,start,duration);
    b->size=0;return ret;
}
int nova_media_stream(Cancel cancel, void *opaque, Read read, Seek seek, void *io, int selected_audio_index, double start_position, int64_t limit, Emit emit, void *emit_opaque) {
    Job job = {cancel,opaque,limit,read,seek,io,NULL,0}; AVFormatContext *in = NULL, *out = NULL;
    AVPacket *packet = NULL; AVBSFContext **filters = NULL; int *map = NULL; StreamBuffer buffer={.limit=limit,.emit=emit,.opaque=emit_opaque,.job=&job};
    AVPacket *prelude[256]={0};int prelude_count=0,prelude_cursor=0;
    int64_t *last_dts = NULL,*audio_anchor=NULL,*audio_samples=NULL; int ret = open_input(NULL, &job, &in), opened = 0;
    if (ret < 0) goto cleanup;
    int audio_index=selected_audio_index;
    if(audio_index<0)audio_index=default_audio(in);
    AVCodecParameters *audio=audio_index>=0 && (unsigned)audio_index<in->nb_streams?in->streams[audio_index]->codecpar:NULL;
    int video_index=-1;for(unsigned i=0;i<in->nb_streams;i++)if(in->streams[i]->codecpar->codec_type==AVMEDIA_TYPE_VIDEO&&!(in->streams[i]->disposition&AV_DISPOSITION_ATTACHED_PIC)){video_index=i;break;}
    AVCodecParameters *video=video_index>=0?in->streams[video_index]->codecpar:NULL;
    int audio_adts=audio&&audio->codec_id==AV_CODEC_ID_AAC&&!audio->extradata_size;
    int64_t prelude_bytes=0;
    while((audio&&audio->codec_id==AV_CODEC_ID_AAC&&(!audio->sample_rate||!channel_count(audio))) || (video&&(video->codec_id==AV_CODEC_ID_H264||video->codec_id==AV_CODEC_ID_HEVC)&&!video->extradata_size)){
        if(prelude_count==256||prelude_bytes>8388608||interrupted(&job)){ret=AVERROR_INVALIDDATA;goto cleanup;}
        AVPacket *p=av_packet_alloc();if(!p){ret=AVERROR(ENOMEM);goto cleanup;}
        ret=av_read_frame(in,p);if(ret<0){av_packet_free(&p);goto cleanup;}
        if(p->size>FFMIN(8388608,limit)-prelude_bytes){av_packet_free(&p);ret=AVERROR(ENOSPC);goto cleanup;}
        prelude[prelude_count++]=p;prelude_bytes+=p->size;
        if(p->stream_index==audio_index&&audio&&audio->codec_id==AV_CODEC_ID_AAC&&(!audio->sample_rate||!channel_count(audio))){ret=adts_parameters(audio,p);if(ret<0){goto cleanup;}}
        if(p->stream_index==video_index&&video&&!video->extradata_size){ret=extract_parameters(video,p);if(ret<0){goto cleanup;}}
    }
    /* Complete AAC configuration before the nonseekable MP4 header. */
    if(audio && audio->codec_id==AV_CODEC_ID_AAC && !audio->extradata_size){
        static const int rates[]={96000,88200,64000,48000,44100,32000,24000,22050,16000,12000,11025,8000,7350};
        int rate=0;while(rate<13&&rates[rate]!=audio->sample_rate)rate++;
        int channels=channel_count(audio);if(channels==8)channels=7;
        if(rate==13||channels<1||channels>7){ret=AVERROR_INVALIDDATA;goto cleanup;}
        audio->extradata=av_mallocz(2+AV_INPUT_BUFFER_PADDING_SIZE);if(!audio->extradata){ret=AVERROR(ENOMEM);goto cleanup;}
        audio->extradata[0]=(2<<3)|(rate>>1);audio->extradata[1]=(rate<<7)|(channels<<3);audio->extradata_size=2;
    }
    /* Seek uses the source's presentation timeline, whose zero is start_time. */
    double source_zero=in->start_time==AV_NOPTS_VALUE?0.0:(double)in->start_time/AV_TIME_BASE;
    if(in->duration!=AV_NOPTS_VALUE && start_position>(double)in->duration/AV_TIME_BASE){ret=AVERROR(ENOSYS);goto cleanup;}
    if(start_position>0){
        for(int i=0;i<prelude_count;i++){av_packet_free(&prelude[i]);}
        prelude_count=0;
        int64_t target=(int64_t)((start_position+source_zero)*AV_TIME_BASE);
        ret=avformat_seek_file(in,-1,INT64_MIN,target,target,AVSEEK_FLAG_BACKWARD);
        if(ret<0)goto cleanup;
    }
    /* Find a decodable starting keyframe. Bound skipped packets and bytes. */
    AVPacket *first=NULL;int scanned=0;int64_t scanned_bytes=0;
    AVPacket *leading[256]={0};int leading_count=0;
    while(!first){
        AVPacket *p=av_packet_alloc();if(!p){ret=AVERROR(ENOMEM);break;}
        if(prelude_cursor<prelude_count){av_packet_move_ref(p,prelude[prelude_cursor++]);ret=0;}else ret=av_read_frame(in,p);
        if(ret<0){av_packet_free(&p);break;}
        scanned_bytes+=p->size;
        int wanted=video_index>=0?p->stream_index==video_index&&(p->flags&AV_PKT_FLAG_KEY):p->stream_index==audio_index;
        if(wanted)first=p;
        else if(p->stream_index==audio_index && leading_count<255)leading[leading_count++]=p;
        else av_packet_free(&p);
        if(++scanned>100000||scanned_bytes>limit||leading_count==255||interrupted(&job)){ret=scanned_bytes>limit?AVERROR(ENOSPC):AVERROR_INVALIDDATA;break;}
    }
    if(ret<0||!first){for(int i=0;i<leading_count;i++)av_packet_free(&leading[i]);av_packet_free(&first);goto cleanup;}
    if(first->dts==AV_NOPTS_VALUE||first->pts==AV_NOPTS_VALUE){for(int i=0;i<leading_count;i++)av_packet_free(&leading[i]);av_packet_free(&first);ret=AVERROR_INVALIDDATA;goto cleanup;}
    AVRational anchor_tb=in->streams[first->stream_index]->time_base;
    int64_t anchor=first->dts;
    for(int i=0;i<leading_count;i++){
        AVPacket *p=leading[i];AVRational tb=in->streams[p->stream_index]->time_base;
        if(p->dts!=AV_NOPTS_VALUE&&av_compare_ts(p->dts,tb,anchor,anchor_tb)<0){anchor=p->dts;anchor_tb=tb;}
    }
    double origin=anchor*av_q2d(anchor_tb);
    buffer.origin=origin-source_zero;
    int remaining=prelude_count-prelude_cursor;
    if((start_position>0 && first->pts*av_q2d(in->streams[first->stream_index]->time_base)-source_zero>start_position+0.05)||remaining+leading_count+1>256){
        for(int i=0;i<leading_count;i++){av_packet_free(&leading[i]);}
        av_packet_free(&first);ret=AVERROR(ENOSYS);goto cleanup;
    }
    for(int i=0;i<prelude_cursor;i++)av_packet_free(&prelude[i]);
    memmove(prelude+leading_count+1,prelude+prelude_cursor,remaining*sizeof(*prelude));
    memcpy(prelude,leading,leading_count*sizeof(*prelude));prelude[leading_count]=first;
    prelude_count=remaining+leading_count+1;prelude_cursor=0;
    ret = avformat_alloc_output_context2(&out,NULL,"mp4",NULL); if (ret < 0) goto cleanup;
    if(!out){ret=AVERROR(ENOMEM);goto cleanup;}
    out->interrupt_callback.callback = interrupted; out->interrupt_callback.opaque = &job;
    map = av_malloc_array(in->nb_streams,sizeof(*map));
    filters = av_calloc(in->nb_streams,sizeof(*filters));
    last_dts = av_malloc_array(in->nb_streams,sizeof(*last_dts));
    audio_anchor=av_malloc_array(in->nb_streams,sizeof(*audio_anchor));audio_samples=av_calloc(in->nb_streams,sizeof(*audio_samples));
    if (!map || !filters || !last_dts || !audio_anchor || !audio_samples) { ret = AVERROR(ENOMEM); goto cleanup; }
    int selected_video = 0, selected_audio = 0;
    for(unsigned i=0;i<in->nb_streams;i++) {
        AVStream *s = in->streams[i]; AVCodecParameters *p = s->codecpar; map[i] = -1; last_dts[i] = AV_NOPTS_VALUE;audio_anchor[i]=AV_NOPTS_VALUE;
        if (p->codec_type == AVMEDIA_TYPE_VIDEO && !(s->disposition & AV_DISPOSITION_ATTACHED_PIC) && !selected_video) selected_video=1;
        else if (p->codec_type == AVMEDIA_TYPE_AUDIO && !selected_audio && (audio_index<0 || i==(unsigned)audio_index)) selected_audio=1;
        else continue;
        if (p->codec_id != AV_CODEC_ID_H264 && p->codec_id != AV_CODEC_ID_HEVC && p->codec_id != AV_CODEC_ID_AV1 && p->codec_id != AV_CODEC_ID_AAC && p->codec_id != AV_CODEC_ID_MP3) {ret=AVERROR(ENOSYS);goto cleanup;}
        AVStream *d = avformat_new_stream(out,NULL); if (!d) {ret=AVERROR(ENOMEM);goto cleanup;}
        map[i] = d->index;
        const char *filter = NULL;
        if (p->codec_id == AV_CODEC_ID_AAC && audio_adts) filter = "aac_adtstoasc";
        /* Annex B AVC/HEVC needs decoder configuration extracted before MP4 header. */
        if ((p->codec_id == AV_CODEC_ID_H264 || p->codec_id == AV_CODEC_ID_HEVC) && !p->extradata_size) {ret=AVERROR_INVALIDDATA;goto cleanup;}
        if (filter) {
            const AVBitStreamFilter *bsf = av_bsf_get_by_name(filter); if (!bsf) {ret=AVERROR(ENOSYS);goto cleanup;}
            ret=av_bsf_alloc(bsf,&filters[i]); if(ret<0)goto cleanup;
            ret=avcodec_parameters_copy(filters[i]->par_in,p); if(ret<0)goto cleanup;
            filters[i]->time_base_in=s->time_base;
            ret=av_bsf_init(filters[i]); if(ret<0)goto cleanup;
            ret=avcodec_parameters_copy(d->codecpar,filters[i]->par_out);
        } else ret=avcodec_parameters_copy(d->codecpar,p);
        if(ret<0)goto cleanup;
        d->codecpar->codec_tag=0; d->time_base=s->time_base;
        if (p->codec_id==AV_CODEC_ID_HEVC) d->codecpar->codec_tag=MKTAG('h','v','c','1');
    }
    if(selected_audio_index>=0 && !selected_audio){ret=AVERROR_STREAM_NOT_FOUND;goto cleanup;}
    if(!selected_video && !selected_audio) {ret=AVERROR_STREAM_NOT_FOUND;goto cleanup;}
    uint8_t *write_buffer=av_malloc(32768);
    if(!write_buffer){ret=AVERROR(ENOMEM);goto cleanup;}
    out->pb=avio_alloc_context(write_buffer,32768,1,&buffer,NULL,stream_write,NULL);
    if(!out->pb){av_free(write_buffer);ret=AVERROR(ENOMEM);goto cleanup;}
    opened=1;out->pb->seekable=0;out->flags|=AVFMT_FLAG_CUSTOM_IO;
    out->avoid_negative_ts=AVFMT_AVOID_NEG_TS_DISABLED;
    out->max_interleave_delta=AV_TIME_BASE;
    AVDictionary *opts=NULL;
    av_dict_set(&opts,"movflags","empty_moov+delay_moov+default_base_moof+frag_custom+skip_trailer",0);
    av_dict_set(&opts,"use_editlist","1",0);
    ret=avformat_write_header(out,&opts);av_dict_free(&opts);if(ret<0)goto cleanup;
    avio_flush(out->pb);if(out->pb->error<0){ret=out->pb->error;goto cleanup;}
    /* delay_moov waits until every selected track has its first timestamp. */
    double fragment_start=0,fragment_end=0;int64_t fragment_bytes=0;int fragment_packets=0,fragment_has_video=0;
    packet=av_packet_alloc(); if(!packet){ret=AVERROR(ENOMEM);goto cleanup;}
    while(1) {
        if(prelude_cursor<prelude_count){av_packet_move_ref(packet,prelude[prelude_cursor++]);ret=0;}else ret=av_read_frame(in,packet);
        if(ret<0)break;
        if(interrupted(&job)){ret=AVERROR_EXIT;goto cleanup;}
        unsigned idx=packet->stream_index;
        if(map[idx]<0){av_packet_unref(packet);continue;}
        AVStream *s=in->streams[idx],*d=out->streams[map[idx]];

        if(d->codecpar->codec_id==AV_CODEC_ID_AAC && filters[idx]){
            AVCodecParameters configuration={0};ret=adts_parameters(&configuration,packet);
            int matches=ret>=0&&configuration.sample_rate==d->codecpar->sample_rate&&channel_count(&configuration)==channel_count(d->codecpar);
            clear_channels(&configuration);
            if(!matches){ret=AVERROR_INVALIDDATA;goto cleanup;}
        }
        if(d->codecpar->codec_id==AV_CODEC_ID_AAC && packet->duration<=0 && d->codecpar->frame_size<=0){ret=AVERROR_INVALIDDATA;goto cleanup;}
        /* AAC has no reorder delay. Missing timestamps after a known anchor can
         * be recovered from its signalled sample count without drift. Video DTS
         * is never synthesized, especially for ambiguous Matroska B frames. */
        if(d->codecpar->codec_id==AV_CODEC_ID_AAC && d->codecpar->sample_rate>0 && d->codecpar->frame_size>0){
            AVRational samples={1,d->codecpar->sample_rate};
            if(packet->dts==AV_NOPTS_VALUE&&packet->pts!=AV_NOPTS_VALUE)packet->dts=packet->pts;
            if(packet->dts!=AV_NOPTS_VALUE){audio_anchor[idx]=packet->dts;audio_samples[idx]=0;}
            else if(audio_anchor[idx]!=AV_NOPTS_VALUE)packet->dts=audio_anchor[idx]+av_rescale_q(audio_samples[idx],samples,s->time_base);
            if(packet->pts==AV_NOPTS_VALUE)packet->pts=packet->dts;
            if(packet->duration<=0)packet->duration=av_rescale_q(audio_samples[idx]+d->codecpar->frame_size,samples,s->time_base)-av_rescale_q(audio_samples[idx],samples,s->time_base);
            audio_samples[idx]+=d->codecpar->frame_size;
        }
        if(packet->dts==AV_NOPTS_VALUE || packet->pts==AV_NOPTS_VALUE || (last_dts[idx]!=AV_NOPTS_VALUE && packet->dts<=last_dts[idx])) {ret=AVERROR_INVALIDDATA;goto cleanup;}
        last_dts[idx]=packet->dts;
        /* All tracks use the same source DTS origin: never reset audio separately. */
        int64_t shift=av_rescale_q(anchor,anchor_tb,s->time_base);
        packet->dts-=shift;packet->pts-=shift;
        if(packet->dts<0){av_packet_unref(packet);continue;}
        double pts=packet->pts*av_q2d(s->time_base);
        double end=pts+packet->duration*av_q2d(s->time_base);
        int boundary=video_index>=0?(idx==(unsigned)video_index&&(packet->flags&AV_PKT_FLAG_KEY)):(pts-fragment_start>=2.0);
        if(boundary&&fragment_packets&&(video_index<0||fragment_has_video)){
            ret=av_interleaved_write_frame(out,NULL);if(ret<0)goto cleanup;
            ret=av_write_frame(out,NULL);if(ret<0)goto cleanup;
            if(!buffer.initialized){ret=av_write_frame(out,NULL);if(ret<0)goto cleanup;}
            avio_flush(out->pb);if(out->pb->error<0){ret=out->pb->error;goto cleanup;}
            ret=stream_emit(&buffer,1,fragment_start,pts-fragment_start);if(ret<0)goto cleanup;
            fragment_start=pts;fragment_end=pts;fragment_bytes=0;fragment_packets=0;
        }
        if(packet->size+128LL>limit/2-fragment_bytes){ret=AVERROR(ENOSPC);goto cleanup;}
        fragment_bytes+=packet->size+128LL;fragment_packets++;
        if(idx==(unsigned)video_index)fragment_has_video=1;
        if(end>fragment_end)fragment_end=end;

        if(filters[idx]) {
            ret=av_bsf_send_packet(filters[idx],packet); if(ret<0){goto cleanup;}
            while((ret=av_bsf_receive_packet(filters[idx],packet))>=0) {
                av_packet_rescale_ts(packet,filters[idx]->time_base_out,d->time_base);packet->stream_index=d->index;packet->pos=-1;
                ret=av_interleaved_write_frame(out,packet);if(ret<0)goto cleanup;
            }
            if(ret!=AVERROR(EAGAIN)&&ret!=AVERROR_EOF){goto cleanup;}
        } else {
            av_packet_rescale_ts(packet,s->time_base,d->time_base);packet->stream_index=d->index;packet->pos=-1;
            ret=av_interleaved_write_frame(out,packet);if(ret<0)goto cleanup;
        }

    }
    if(ret!=AVERROR_EOF)goto cleanup;
    for(unsigned i=0;i<in->nb_streams;i++) if(filters[i]) {
        ret=av_bsf_send_packet(filters[i],NULL);if(ret<0)goto cleanup;
        while((ret=av_bsf_receive_packet(filters[i],packet))>=0){AVStream *d=out->streams[map[i]];av_packet_rescale_ts(packet,filters[i]->time_base_out,d->time_base);packet->stream_index=d->index;packet->pos=-1;ret=av_interleaved_write_frame(out,packet);if(ret<0){goto cleanup;}}
        if(ret!=AVERROR_EOF&&ret!=AVERROR(EAGAIN))goto cleanup;
    }
    ret=av_write_trailer(out);
    if(ret>=0){avio_flush(out->pb);if(out->pb->error<0)ret=out->pb->error;}
    if(ret>=0&&fragment_packets)ret=stream_emit(&buffer,1,fragment_start,fragment_end-fragment_start);
    if(ret>=0)ret=stream_emit(&buffer,2,0,0);
cleanup:
    for(int i=0;i<prelude_count;i++)av_packet_free(&prelude[i]);
    av_packet_free(&packet);
    if(filters&&in){for(unsigned i=0;i<in->nb_streams;i++)av_bsf_free(&filters[i]);}
    av_free(filters);av_free(map);av_free(last_dts);av_free(audio_anchor);av_free(audio_samples);
    if(opened&&out->pb){av_freep(&out->pb->buffer);avio_context_free(&out->pb);}
    av_free(buffer.bytes);
    avformat_free_context(out);avformat_close_input(&in);
    if(job.avio) {av_freep(&job.avio->buffer);avio_context_free(&job.avio);}
    return ret;
}
